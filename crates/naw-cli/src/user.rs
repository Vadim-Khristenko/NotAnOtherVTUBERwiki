//! `naw user`: accounts from the command line.
//!
//! The first account on an invite-only install cannot come from the admin
//! panel, because the panel needs an admin, and production runs without the
//! dev sign-in. This is that first step, and later the recovery path for a root
//! account, which the web panel refuses to reset on purpose.
//!
//! Temporary passwords are printed to the terminal and nowhere else. Whoever
//! runs this owns the database already, so printing is not a new exposure.

use sqlx::PgPool;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_web::credentials;

/// Why a command did not run. A refusal carries its reason to the terminal;
/// `AppError` keeps its detail out of web pages on purpose, which is the wrong
/// call for somebody reading a prompt.
pub enum Failure {
    Refused(String),
    App(AppError),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => f.write_str(reason),
            Self::App(err) => write!(f, "{err}"),
        }
    }
}

impl<E: Into<AppError>> From<E> for Failure {
    fn from(err: E) -> Self {
        Self::App(err.into())
    }
}

pub const USAGE: &str = "usage:\n  \
    naw user create --username NAME [--email ADDR] [--trust-email] [--install <staff|root>]\n  \
    naw user reset-password --username NAME\n\
  \n\
  Both print a temporary password. Its owner signs in with it once and has to\n\
  choose their own before anything else. Reserved names are allowed here.\n\
  Wiki roles are given with `naw grant`.";

pub enum Command {
    Create {
        username: String,
        email: Option<String>,
        trust_email: bool,
        install: Option<String>,
    },
    ResetPassword {
        username: String,
    },
}

pub fn parse(raw: &[String]) -> Result<Command, String> {
    let (verb, rest) = raw.split_first().ok_or("missing subcommand")?;
    let mut username = None;
    let mut email = None;
    let mut trust_email = false;
    let mut install = None;
    let mut i = 0;
    while i < rest.len() {
        let value = |i: usize| {
            rest.get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{} needs a value", rest[i]))
        };
        match rest[i].as_str() {
            "--username" | "--user" => {
                username = Some(value(i)?);
                i += 1;
            }
            "--email" => {
                email = Some(value(i)?);
                i += 1;
            }
            "--install" => {
                install = Some(value(i)?);
                i += 1;
            }
            "--trust-email" => trust_email = true,
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    let username = username
        .map(|name| name.trim().to_lowercase())
        .ok_or("--username is required")?;
    match verb.as_str() {
        "create" => {
            if let Some(role) = &install
                && !matches!(role.as_str(), "staff" | "root")
            {
                return Err(format!("--install must be staff or root, got {role}"));
            }
            Ok(Command::Create {
                username,
                email: email
                    .map(|e| e.trim().to_lowercase())
                    .filter(|e| !e.is_empty()),
                trust_email,
                install,
            })
        }
        "reset-password" => Ok(Command::ResetPassword { username }),
        other => Err(format!("unknown subcommand: {other}")),
    }
}

async fn audit(
    pool: &PgPool,
    action: &str,
    user_id: Uuid,
    meta: serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query!(
        "INSERT INTO audit_log (id, wiki_id, user_id, action, entity_type, entity_id, meta)
         VALUES ($1, NULL, NULL, $2, 'user', $3, $4)",
        Uuid::new_v4(),
        action,
        user_id,
        meta
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Runs the command and returns what to print.
pub async fn run(pool: &PgPool, command: Command) -> Result<String, Failure> {
    match command {
        Command::Create {
            username,
            email,
            trust_email,
            install,
        } => {
            if !credentials::username_is_valid(&username) {
                return Err(Failure::Refused(format!(
                    "{username:?} is not a valid username: 3 to 32 characters, a-z 0-9 - _ and dots between them, starting with a letter"
                )));
            }
            let taken = sqlx::query!(
                "SELECT 1 AS one FROM users WHERE lower(username) = $1
                 UNION ALL
                 SELECT 1 AS one FROM user_aliases WHERE lower(alias) = $1",
                username
            )
            .fetch_optional(pool)
            .await?
            .is_some();
            if taken {
                return Err(Failure::Refused(format!("{username} is already taken")));
            }
            let temporary = credentials::temporary();
            let hash = credentials::hash(temporary.clone()).await?;
            let id = Uuid::new_v4();
            let verified_at = email
                .as_ref()
                .filter(|_| trust_email)
                .map(|_| chrono::Utc::now());
            let role = install.as_deref().unwrap_or("registered");
            sqlx::query!(
                "INSERT INTO users (id, username, email, email_verified_at, password_hash,
                                    must_change_password, global_role)
                 VALUES ($1, $2, $3, $4, $5, true, $6)",
                id,
                username,
                email,
                verified_at,
                hash,
                role
            )
            .execute(pool)
            .await?;
            audit(
                pool,
                "cli.user_create",
                id,
                serde_json::json!({ "username": username, "install_role": role }),
            )
            .await?;
            Ok(format!(
                "created {username} ({role})\n\
                 temporary password: {temporary}\n\
                 \n\
                 It is shown only now. On the first sign-in {username} has to choose their own."
            ))
        }
        Command::ResetPassword { username } => {
            let Some(row) =
                sqlx::query!("SELECT id FROM users WHERE lower(username) = $1", username)
                    .fetch_optional(pool)
                    .await?
            else {
                return Err(Failure::Refused(format!("no account named {username}")));
            };
            let temporary = credentials::temporary();
            let hash = credentials::hash(temporary.clone()).await?;
            sqlx::query!(
                "UPDATE users SET password_hash = $2, must_change_password = true WHERE id = $1",
                row.id,
                hash
            )
            .execute(pool)
            .await?;
            let ended = sqlx::query!("DELETE FROM sessions WHERE user_id = $1", row.id)
                .execute(pool)
                .await?
                .rows_affected();
            audit(
                pool,
                "cli.password_reset",
                row.id,
                serde_json::json!({ "username": username, "sessions_ended": ended }),
            )
            .await?;
            Ok(format!(
                "new temporary password for {username}: {temporary}\n\
                 {ended} session(s) ended. It is shown only now."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn create_parses_and_normalizes() {
        let Command::Create {
            username,
            email,
            trust_email,
            install,
        } = parse(&args(&[
            "create",
            "--username",
            " VAI ",
            "--email",
            "Vadim@Filian.Wiki",
            "--trust-email",
            "--install",
            "root",
        ]))
        .expect("parses")
        else {
            panic!("expected create");
        };
        assert_eq!(username, "vai");
        assert_eq!(email.as_deref(), Some("vadim@filian.wiki"));
        assert!(trust_email);
        assert_eq!(install.as_deref(), Some("root"));
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["create"])).is_err());
        assert!(parse(&args(&["create", "--username"])).is_err());
        assert!(parse(&args(&["create", "--username", "a", "--install", "owner"])).is_err());
        assert!(parse(&args(&["delete", "--username", "a"])).is_err());
        assert!(parse(&args(&["create", "--username", "a", "--what"])).is_err());
    }
}

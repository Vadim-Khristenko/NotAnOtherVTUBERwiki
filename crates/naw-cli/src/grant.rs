//! `naw grant`: the bootstrap path for privileges.
//!
//! Needed because the admin panel needs an admin to reach it, and a fresh
//! install has none. Somebody with shell access has to be able to appoint the
//! first one, and that somebody already owns the database, so this grants
//! privileges without asking for any of its own.
//!
//! Deliberately not reachable over HTTP. `install` sets `users.global_role`,
//! which reaches every wiki on the install, and that is not a decision a web
//! form should be able to make.

use sqlx::PgPool;
use uuid::Uuid;

use naw_core::error::AppError;

pub const USAGE: &str = "usage:\n  \
    naw grant --user NAME --wiki SLUG --role <registered|sponsor|curator|moderator|admin|owner>\n  \
    naw grant --user NAME --install <registered|staff|root>\n  \
    naw grant --user NAME --wiki SLUG --revoke\n\
  \n\
  --role     a per-wiki membership\n  \
  --install  a cross-wiki role: staff moderates everywhere, root holds every\n             \
             capability on every wiki. Use root for the operator account.";

/// Per-wiki roles as the `user_wiki_role` enum spells them.
const WIKI_ROLES: [&str; 6] = [
    "registered",
    "sponsor",
    "curator",
    "moderator",
    "admin",
    "owner",
];

/// Install-wide roles the CHECK constraint allows.
const INSTALL_ROLES: [&str; 3] = ["registered", "staff", "root"];

pub struct Args {
    pub user: String,
    pub wiki: Option<String>,
    pub role: Option<String>,
    pub install: Option<String>,
    pub revoke: bool,
}

/// Parses the flags. Returns the usage text as the error, so the caller prints
/// one thing and exits.
pub fn parse(raw: &[String]) -> Result<Args, String> {
    let mut args = Args {
        user: String::new(),
        wiki: None,
        role: None,
        install: None,
        revoke: false,
    };
    let mut i = 0;
    while i < raw.len() {
        let flag = raw[i].as_str();
        let value = || -> Result<String, String> {
            raw.get(i + 1)
                .filter(|next| !next.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--user" => {
                args.user = value()?;
                i += 2;
            }
            "--wiki" => {
                args.wiki = Some(value()?);
                i += 2;
            }
            "--role" => {
                args.role = Some(value()?);
                i += 2;
            }
            "--install" => {
                args.install = Some(value()?);
                i += 2;
            }
            "--revoke" => {
                args.revoke = true;
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if args.user.trim().is_empty() {
        return Err("--user is required".to_string());
    }
    // Every combination that is not exactly one action is refused, rather than
    // silently picking one. Granting a role and revoking it in one command is a
    // typo, not a request.
    let actions = usize::from(args.role.is_some())
        + usize::from(args.install.is_some())
        + usize::from(args.revoke);
    if actions != 1 {
        return Err("give exactly one of --role, --install or --revoke".to_string());
    }
    if (args.role.is_some() || args.revoke) && args.wiki.is_none() {
        return Err("--role and --revoke need --wiki".to_string());
    }
    if let Some(role) = &args.role
        && !WIKI_ROLES.contains(&role.as_str())
    {
        return Err(format!("--role must be one of: {}", WIKI_ROLES.join(", ")));
    }
    if let Some(role) = &args.install
        && !INSTALL_ROLES.contains(&role.as_str())
    {
        return Err(format!(
            "--install must be one of: {}",
            INSTALL_ROLES.join(", ")
        ));
    }
    Ok(args)
}

async fn find_user(pool: &PgPool, username: &str) -> Result<Option<Uuid>, AppError> {
    // Usernames are compared case insensitively, because that is how somebody
    // types one from memory at a shell prompt.
    Ok(sqlx::query!(
        "SELECT id FROM users WHERE lower(username) = lower($1)",
        username
    )
    .fetch_optional(pool)
    .await?
    .map(|row| row.id))
}

async fn find_wiki(pool: &PgPool, slug: &str) -> Result<Option<Uuid>, AppError> {
    Ok(sqlx::query!("SELECT id FROM wikis WHERE slug = $1", slug)
        .fetch_optional(pool)
        .await?
        .map(|row| row.id))
}

pub async fn run(pool: &PgPool, args: &Args) -> Result<String, AppError> {
    let Some(user_id) = find_user(pool, &args.user).await? else {
        return Err(AppError::Config(format!("no account named {}", args.user)));
    };

    if let Some(install_role) = &args.install {
        sqlx::query!(
            "UPDATE users SET global_role = $2 WHERE id = $1",
            user_id,
            install_role
        )
        .execute(pool)
        .await?;
        write_audit(pool, None, user_id, "grant.install", install_role).await?;
        return Ok(format!(
            "{} is now {install_role} across the install",
            args.user
        ));
    }

    // Both remaining actions need a wiki.
    let slug = args.wiki.as_deref().unwrap_or_default();
    let Some(wiki_id) = find_wiki(pool, slug).await? else {
        return Err(AppError::Config(format!("no wiki with slug {slug}")));
    };

    if args.revoke {
        let removed = sqlx::query!(
            "DELETE FROM wiki_memberships WHERE user_id = $1 AND wiki_id = $2",
            user_id,
            wiki_id
        )
        .execute(pool)
        .await?
        .rows_affected();
        if removed == 0 {
            return Ok(format!("{} had no membership on {slug}", args.user));
        }
        write_audit(pool, Some(wiki_id), user_id, "grant.revoke", slug).await?;
        return Ok(format!("{} is no longer a member of {slug}", args.user));
    }

    let role = args.role.as_deref().unwrap_or_default();
    // A plain query with a bind and an explicit enum cast: the value came from
    // the WIKI_ROLES allowlist in `parse`, and the macro cannot type a user
    // defined enum without a custom Rust type.
    sqlx::query(
        "INSERT INTO wiki_memberships (user_id, wiki_id, role)
         VALUES ($1, $2, $3::user_wiki_role)
         ON CONFLICT (user_id, wiki_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(user_id)
    .bind(wiki_id)
    .bind(role)
    .execute(pool)
    .await?;
    write_audit(pool, Some(wiki_id), user_id, "grant.role", role).await?;
    Ok(format!("{} is now {role} on {slug}", args.user))
}

/// Records the grant.
///
/// `user_id` is the account that was changed, not an actor: a command run from
/// a shell has no signed-in actor to attribute it to, and inventing one would
/// be worse than leaving the column null. The audit viewer shows these as
/// anonymous, which is the truth.
async fn write_audit(
    pool: &PgPool,
    wiki_id: Option<Uuid>,
    subject: Uuid,
    action: &str,
    detail: &str,
) -> Result<(), AppError> {
    sqlx::query!(
        "INSERT INTO audit_log (id, wiki_id, user_id, action, entity_type, entity_id, meta)
         VALUES ($1, $2, NULL, $3, 'user', $4, $5)",
        Uuid::new_v4(),
        wiki_id,
        action,
        subject,
        serde_json::json!({ "detail": detail, "via": "cli" })
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_wiki_grant_parses() {
        let args = parse(&flags(&[
            "--user", "vai", "--wiki", "filian", "--role", "owner",
        ]))
        .expect("valid");
        assert_eq!(args.user, "vai");
        assert_eq!(args.wiki.as_deref(), Some("filian"));
        assert_eq!(args.role.as_deref(), Some("owner"));
        assert!(!args.revoke);
    }

    #[test]
    fn an_install_grant_needs_no_wiki() {
        let args = parse(&flags(&["--user", "vai", "--install", "root"])).expect("valid");
        assert_eq!(args.install.as_deref(), Some("root"));
        assert_eq!(args.wiki, None);
    }

    #[test]
    fn exactly_one_action_is_required() {
        // No action at all.
        assert!(parse(&flags(&["--user", "vai", "--wiki", "filian"])).is_err());
        // Two actions, which is a typo rather than a request.
        assert!(
            parse(&flags(&[
                "--user", "vai", "--wiki", "filian", "--role", "admin", "--revoke",
            ]))
            .is_err()
        );
        assert!(
            parse(&flags(&[
                "--user",
                "vai",
                "--wiki",
                "filian",
                "--role",
                "admin",
                "--install",
                "root",
            ]))
            .is_err()
        );
    }

    #[test]
    fn wiki_roles_match_the_database_enum() {
        // Every value the migrations give `user_wiki_role`, in any order.
        let dir = format!("{}/../../migrations", env!("CARGO_MANIFEST_DIR"));
        let mut from_sql = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(dir).expect("migrations dir") {
            let sql = std::fs::read_to_string(entry.expect("entry").path()).expect("read");
            let mut rest = sql.as_str();
            while let Some(at) = rest.find("user_wiki_role") {
                rest = &rest[at + "user_wiki_role".len()..];
                let head = rest.trim_start();
                let values = if let Some(list) = head.strip_prefix("AS ENUM") {
                    list.split(')').next().unwrap_or_default()
                } else if let Some(added) = head.strip_prefix("ADD VALUE") {
                    added.split(';').next().unwrap_or_default()
                } else {
                    continue;
                };
                let quoted: Vec<&str> = values.split('\'').collect();
                // Only the quoted value of ADD VALUE, not its BEFORE target.
                let take = if head.starts_with("ADD VALUE") {
                    1
                } else {
                    usize::MAX
                };
                for value in quoted.iter().skip(1).step_by(2).take(take) {
                    from_sql.insert(value.to_string());
                }
            }
        }
        let ours: std::collections::BTreeSet<String> =
            WIKI_ROLES.iter().map(|r| r.to_string()).collect();
        assert_eq!(ours, from_sql);
        assert!(
            parse(&flags(&[
                "--user", "vai", "--wiki", "filian", "--role", "curator"
            ]))
            .is_ok()
        );
    }

    #[test]
    fn a_role_name_outside_the_enum_is_refused_before_the_database_sees_it() {
        for bad in ["god", "Owner", "admin'", "", "owner; drop table users"] {
            let result = parse(&flags(&[
                "--user", "vai", "--wiki", "filian", "--role", bad,
            ]));
            assert!(result.is_err(), "{bad:?} must be refused");
        }
        for bad in ["superuser", "Root", "admin"] {
            let result = parse(&flags(&["--user", "vai", "--install", bad]));
            assert!(result.is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_per_wiki_action_without_a_wiki_is_refused() {
        assert!(parse(&flags(&["--user", "vai", "--role", "admin"])).is_err());
        assert!(parse(&flags(&["--user", "vai", "--revoke"])).is_err());
    }

    #[test]
    fn a_missing_user_or_value_is_refused() {
        assert!(parse(&flags(&["--wiki", "filian", "--role", "admin"])).is_err());
        assert!(parse(&flags(&["--user", "   ", "--install", "root"])).is_err());
        // A flag whose value is the next flag, which is a forgotten argument.
        assert!(parse(&flags(&["--user", "--install", "root"])).is_err());
        assert!(parse(&flags(&["--user", "vai", "--role"])).is_err());
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_ignored() {
        // Silently ignoring a typo like --global would leave the operator
        // thinking they had granted something they had not.
        assert!(parse(&flags(&["--user", "vai", "--global", "root"])).is_err());
    }
}

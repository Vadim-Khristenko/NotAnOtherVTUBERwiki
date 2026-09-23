//! The first owner, from the environment.
//!
//! A fresh invite-only install has nobody who can open the admin panel, and in
//! production the dev sign-in is off (and refuses to run anywhere public). This
//! closes that gap without a back door: the operator names the account and its
//! password in the service's environment, and startup makes sure it exists with
//! every right there is.
//!
//! What startup guarantees, every time:
//! - the account exists, holds the `root` install role, and is `owner` of
//!   every wiki on the install;
//! - it has a password. The one from the environment is written only while the
//!   account has none, so an owner who changed theirs is never reset by a
//!   restart, and a leaked old environment file stops being a key once they do.

use naw_core::config::BootstrapOwner;
use naw_core::error::AppError;
use naw_core::state::AppState;
use uuid::Uuid;

use crate::auth::{password, username};

/// What startup did, for one log line.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Promoted,
    AlreadyOwner,
}

/// Refuses a bootstrap configuration that would create a weak or unusable
/// account. Checked before anything touches the database.
fn validate(owner: &BootstrapOwner) -> Result<(), AppError> {
    if !username::is_valid(&owner.username) {
        return Err(AppError::Config(format!(
            "NAW_BOOTSTRAP_OWNER_USERNAME {:?} is not a valid username: 3 to 32 characters, a-z 0-9 - _, starting with a letter",
            owner.username
        )));
    }
    password::check_new(&owner.password, &owner.password, &owner.username, None).map_err(
        |problem| {
            AppError::Config(format!(
                "the bootstrap owner password is refused ({problem:?}): at least {} characters, not the username, not one repeated character",
                password::MIN_LEN
            ))
        },
    )
}

/// Makes sure the bootstrap owner exists with every right. Returns `None` when
/// no bootstrap owner is configured.
pub async fn ensure_owner(state: &AppState) -> Result<Option<(String, Outcome)>, AppError> {
    let Some(owner) = state.config.bootstrap_owner.as_ref() else {
        return Ok(None);
    };
    validate(owner)?;

    let existing = sqlx::query!(
        "SELECT id, global_role, (password_hash IS NOT NULL) AS \"has_password!\"
         FROM users WHERE lower(username) = $1",
        owner.username
    )
    .fetch_optional(&state.db)
    .await?;

    let mut tx = state.db.begin().await?;
    let (user_id, outcome) = match existing {
        None => {
            let hash = password::hash(owner.password.clone()).await?;
            let id = Uuid::new_v4();
            // Not a temporary password: the operator chose it, so there is
            // nothing to force a change on.
            sqlx::query!(
                "INSERT INTO users (id, username, password_hash, password_changed_at, global_role)
                 VALUES ($1, $2, $3, now(), 'root')",
                id,
                owner.username,
                hash
            )
            .execute(&mut *tx)
            .await?;
            (id, Outcome::Created)
        }
        Some(row) => {
            if !row.has_password {
                let hash = password::hash(owner.password.clone()).await?;
                sqlx::query!(
                    "UPDATE users SET password_hash = $2, password_changed_at = now() WHERE id = $1",
                    row.id,
                    hash
                )
                .execute(&mut *tx)
                .await?;
            }
            let outcome = if row.global_role == "root" {
                Outcome::AlreadyOwner
            } else {
                sqlx::query!(
                    "UPDATE users SET global_role = 'root' WHERE id = $1",
                    row.id
                )
                .execute(&mut *tx)
                .await?;
                Outcome::Promoted
            };
            (row.id, outcome)
        }
    };
    // Owner of every wiki too. Root already holds every capability, but the
    // membership is what the admin panel shows, and it keeps the account the
    // owner if its install role is ever changed by hand.
    sqlx::query(
        "INSERT INTO wiki_memberships (user_id, wiki_id, role)
         SELECT $1, id, 'owner'::user_wiki_role FROM wikis
         ON CONFLICT (user_id, wiki_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    if outcome != Outcome::AlreadyOwner {
        sqlx::query!(
            "INSERT INTO audit_log (id, wiki_id, user_id, action, entity_type, entity_id, meta)
             VALUES ($1, NULL, NULL, 'bootstrap.owner', 'user', $2, $3)",
            Uuid::new_v4(),
            user_id,
            serde_json::json!({ "username": owner.username, "outcome": format!("{outcome:?}") })
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some((owner.username.clone(), outcome)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(username: &str, password: &str) -> BootstrapOwner {
        BootstrapOwner {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    #[test]
    fn weak_or_malformed_bootstrap_owners_are_refused() {
        assert!(validate(&owner("vai", "a long enough password")).is_ok());
        assert!(validate(&owner("VAI Prog", "a long enough password")).is_err());
        assert!(validate(&owner("vai", "short")).is_err());
        assert!(validate(&owner("vaiprog-owner", "vaiprog-owner")).is_err());
        // The refusal must not quote the password back into the log.
        let err = validate(&owner("vai", "tiny"))
            .expect_err("refused")
            .to_string();
        assert!(!err.contains("tiny"), "{err}");
    }
}

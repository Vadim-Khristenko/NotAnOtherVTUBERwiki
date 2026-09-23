//! `finish_login`: one transaction from a provider `Identity` to an account.
//!
//! 1. Known identity: sign in.
//! 2. Link mode: attach to the signed-in account.
//! 3. Auto-link: attach to the account with the same verified email.
//! 4. Otherwise create the account, unless registration is closed.

use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use super::types::{AuthError, Identity};
use super::username;

/// Postgres unique violation.
const UNIQUE_VIOLATION: &str = "23505";

/// `users.email` is unique on `lower(email)`.
fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// How the login resolved, for the audit trail.
#[derive(Debug)]
pub enum Outcome {
    Login(Uuid),
    Register(Uuid),
    Link(Uuid),
}

/// Attaches the identity row to `user_id` inside the transaction.
async fn attach_identity(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    identity: &Identity,
) -> Result<(), AuthError> {
    sqlx::query!(
        r#"
        INSERT INTO oauth_identities
          (id, user_id, provider, provider_user_id, email, display_name,
           avatar_url, raw, last_login_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
        "#,
        Uuid::new_v4(),
        user_id,
        identity.provider.as_str(),
        identity.provider_user_id,
        identity.email,
        identity.display_name,
        identity.avatar_url,
        identity.raw,
    )
    .execute(tx)
    .await
    .map_err(|err| {
        // Only a unique violation means the identity is taken.
        let conflict = err
            .as_database_error()
            .and_then(|db| db.code())
            .is_some_and(|code| code == UNIQUE_VIOLATION);
        if conflict {
            AuthError::BadRequest("identity already belongs to another account")
        } else {
            AuthError::Upstream("identity insert failed".to_string())
        }
    })?;
    Ok(())
}

async fn audit(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    action: &str,
    provider: &str,
) -> Result<(), AppError> {
    sqlx::query!(
        "INSERT INTO audit_log (id, wiki_id, user_id, action, entity_type, meta)
         VALUES ($1, NULL, $2, $3, 'user', $4)",
        Uuid::new_v4(),
        user_id,
        action,
        json!({ "provider": provider })
    )
    .execute(tx)
    .await?;
    Ok(())
}

pub async fn finish_login(
    state: &AppState,
    identity: &Identity,
    link_user_id: Option<Uuid>,
    auto_link_allowed: bool,
) -> Result<Outcome, AuthError> {
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|_| AuthError::Upstream("transaction start failed".to_string()))?;

    // 1. Known identity.
    if let Some(row) = sqlx::query!(
        "SELECT user_id FROM oauth_identities
         WHERE provider = $1 AND provider_user_id = $2",
        identity.provider.as_str(),
        identity.provider_user_id
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AuthError::Upstream("identity lookup failed".to_string()))?
    {
        sqlx::query!(
            "UPDATE oauth_identities SET last_login_at = now()
             WHERE provider = $1 AND provider_user_id = $2",
            identity.provider.as_str(),
            identity.provider_user_id
        )
        .execute(&mut *tx)
        .await
        .map_err(|_| AuthError::Upstream("identity touch failed".to_string()))?;
        audit(
            &mut tx,
            row.user_id,
            "auth.login",
            identity.provider.as_str(),
        )
        .await
        .map_err(|err| AuthError::Upstream(err.to_string()))?;
        tx.commit()
            .await
            .map_err(|_| AuthError::Upstream("commit failed".to_string()))?;
        return Ok(Outcome::Login(row.user_id));
    }

    // 2. Link mode.
    if let Some(user_id) = link_user_id {
        let exists = sqlx::query!("SELECT 1 AS one FROM users WHERE id = $1", user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AuthError::Upstream("user lookup failed".to_string()))?;
        if exists.is_none() {
            return Err(AuthError::BadRequest("link target missing"));
        }
        attach_identity(&mut tx, user_id, identity).await?;
        audit(&mut tx, user_id, "auth.link", identity.provider.as_str())
            .await
            .map_err(|err| AuthError::Upstream(err.to_string()))?;
        tx.commit()
            .await
            .map_err(|_| AuthError::Upstream("commit failed".to_string()))?;
        return Ok(Outcome::Link(user_id));
    }

    // 3. Auto-link. Both sides must be verified, or anyone could park an
    // unverified `victim@host` and collect the victim's identity later.
    let certified = match (&identity.email, identity.email_verified, auto_link_allowed) {
        (Some(email), true, true) => Some(normalize_email(email)),
        _ => None,
    };
    if let Some(email) = certified {
        let row = sqlx::query!(
            "SELECT id FROM users
             WHERE lower(email) = $1 AND email_verified_at IS NOT NULL",
            email
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| AuthError::Upstream("email lookup failed".to_string()))?;
        if let Some(row) = row {
            attach_identity(&mut tx, row.id, identity).await?;
            audit(&mut tx, row.id, "auth.link", identity.provider.as_str())
                .await
                .map_err(|err| AuthError::Upstream(err.to_string()))?;
            tx.commit()
                .await
                .map_err(|_| AuthError::Upstream("commit failed".to_string()))?;
            return Ok(Outcome::Link(row.id));
        }
    }

    // 4. New account. Closed registration stops here, before any write; the
    // dev provider is exempt.
    let closed = state.config.auth.registration == naw_core::config::Registration::Closed;
    if closed && identity.provider != super::types::ProviderId::Dev {
        tracing::info!(
            provider = identity.provider.as_str(),
            "sign-in refused: registration is closed and the identity has no account"
        );
        return Err(AuthError::RegistrationClosed);
    }
    // An unverified email is not written to `users.email`, where it would
    // block its real owner.
    let user_id = Uuid::new_v4();
    let email = identity
        .email
        .as_deref()
        .filter(|_| identity.email_verified)
        .map(normalize_email);
    let username = username::claim(
        &mut tx,
        &state.config.auth.reserved_usernames,
        &identity.handle,
    )
    .await
    .map_err(|_| AuthError::Upstream("username allocation failed".to_string()))?;
    let email_verified_at = email.as_ref().map(|_| chrono::Utc::now());
    sqlx::query!(
        "INSERT INTO users (id, username, email, email_verified_at, global_role)
         VALUES ($1, $2, $3, $4, 'registered')",
        user_id,
        username,
        email,
        email_verified_at
    )
    .execute(&mut *tx)
    .await
    .map_err(|_| AuthError::Upstream("user insert failed".to_string()))?;
    attach_identity(&mut tx, user_id, identity).await?;
    audit(
        &mut tx,
        user_id,
        "auth.register",
        identity.provider.as_str(),
    )
    .await
    .map_err(|err| AuthError::Upstream(err.to_string()))?;
    tx.commit()
        .await
        .map_err(|_| AuthError::Upstream("commit failed".to_string()))?;
    tracing::info!(username = %username, provider = identity.provider.as_str(), "registered");
    Ok(Outcome::Register(user_id))
}

//! `finish_login`: one transaction that turns a provider `Identity` into
//! a signed-in user.
//!
//! Four paths, per the guide:
//! 1. known identity: touch `last_login_at`, log in as that user;
//! 2. unknown identity in link mode: attach to the session user;
//! 3. unknown identity, auto-link on and the email is provider certified:
//!    attach to the user with that email;
//! 4. otherwise: create the user plus the identity row, unless registration
//!    is closed, in which case nothing is written and the caller shows the
//!    "no account yet" page.
//!
//! Tokens never reach this module: `Identity.raw` is a trimmed profile.

use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use super::types::{AuthError, Identity};
use super::username;

/// Postgres unique violation. Anything else from the driver is our bug.
const UNIQUE_VIOLATION: &str = "23505";

/// The `users.email` column is unique on `lower(email)`, so every write goes
/// through here first.
fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// How the login resolved, for the audit trail.
#[derive(Debug)]
pub enum Outcome {
    /// Existing account signed in.
    Login(Uuid),
    /// Fresh account created.
    Register(Uuid),
    /// Identity attached to an existing account.
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
        // Only 23505 on UNIQUE (provider, provider_user_id) means the
        // identity is taken. Any other database error is our problem, not
        // the user's, and must not be reported as a conflict.
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

    // 1. Known identity, straight in.
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

    // 2. Link mode: attach to the signed-in user.
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

    // 3. Auto-link on a provider certified email.
    //
    // Both sides have to be certified. The provider vouches for the incoming
    // address via `email_verified`, and the target account only counts when
    // it confirmed the same address itself: otherwise anyone could register
    // with an unverified `victim@host`, sit on the row, and collect the
    // victim's real identity the next time they sign in elsewhere.
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

    // 4. Fresh account.
    //
    // Closed registration stops here, after the paths that sign in or link an
    // existing account and before anything is written. The dev provider is
    // exempt: it only runs on a laptop, and a closed laptop is useless.
    let closed = state.config.auth.registration == naw_core::config::Registration::Closed;
    if closed && identity.provider != super::types::ProviderId::Dev {
        tracing::info!(
            provider = identity.provider.as_str(),
            "sign-in refused: registration is closed and the identity has no account"
        );
        return Err(AuthError::RegistrationClosed);
    }
    //
    // An unverified provider email is not written to `users.email`: parking
    // an address nobody proved ownership of would block the real owner from
    // ever claiming it, and the column is unique on lower(email). The
    // address stays on the identity row and the user confirms it in settings.
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

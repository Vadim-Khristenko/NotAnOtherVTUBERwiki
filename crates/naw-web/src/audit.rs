//! Writing to `audit_log`.
//!
//! [`record`] returns the error, for actions whose trail is the point (a
//! role change). [`record_or_log`] logs it instead, for actions that must
//! not fail over bookkeeping. `meta` never holds a secret.

use serde_json::Value;
use uuid::Uuid;

use naw_core::error::AppError;

/// One audit row; `action` reads as `noun.verb`.
pub struct Entry<'a> {
    /// `None` for install-wide events.
    pub wiki_id: Option<Uuid>,
    /// `None` for an anonymous actor.
    pub user_id: Option<Uuid>,
    pub action: &'a str,
    pub entity_type: &'a str,
    pub entity_id: Option<Uuid>,
    pub meta: Value,
}

pub async fn record(db: &sqlx::PgPool, entry: Entry<'_>) -> Result<(), AppError> {
    sqlx::query!(
        "INSERT INTO audit_log (id, wiki_id, user_id, action, entity_type, entity_id, meta)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        Uuid::new_v4(),
        entry.wiki_id,
        entry.user_id,
        entry.action,
        entry.entity_type,
        entry.entity_id,
        entry.meta
    )
    .execute(db)
    .await?;
    Ok(())
}

/// As [`record`], but a failure is logged instead of returned.
pub async fn record_or_log(db: &sqlx::PgPool, entry: Entry<'_>) {
    let action = entry.action;
    if let Err(err) = record(db, entry).await {
        tracing::error!(error = %err, action, "audit row lost");
    }
}

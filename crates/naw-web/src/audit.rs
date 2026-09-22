//! Writing to `audit_log`.
//!
//! Two ways to call this, and the difference matters.
//!
//! `record` returns the error. Use it where the audit row is part of the point:
//! a role change nobody can see afterwards is worse than a role change that
//! failed loudly.
//!
//! `record_or_log` swallows it into a log line. Use it where the action is the
//! point and the trail is bookkeeping: an editor who saved a page must not get
//! a 500 because the audit insert lost a race.
//!
//! Nothing secret goes in `meta`. It ends up in an admin panel, in database
//! dumps and in backups, so it holds slugs, ids and role names, never tokens,
//! never passwords, never a session value.

use serde_json::Value;
use uuid::Uuid;

use naw_core::error::AppError;

/// One audit row. `entity_type` and `action` are the stable machine-readable
/// pair the admin panel filters on: `action` reads as `noun.verb`.
pub struct Entry<'a> {
    /// `None` for install-wide events that belong to no single wiki.
    pub wiki_id: Option<Uuid>,
    /// `None` for an anonymous actor, which is possible on a wiki that allows
    /// anonymous edits.
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

/// As `record`, but a failure becomes a log line instead of a failed request.
pub async fn record_or_log(db: &sqlx::PgPool, entry: Entry<'_>) {
    let action = entry.action;
    if let Err(err) = record(db, entry).await {
        tracing::error!(error = %err, action, "audit row lost");
    }
}

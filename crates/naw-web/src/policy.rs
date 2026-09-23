//! Install-wide rules, with the environment as the default and the admin
//! panel as the override.
//!
//! Read per request rather than cached: these rows change rarely, the query is
//! one primary key lookup, and a cache would mean an admin saves a setting and
//! then watches it not apply.

use naw_core::config::AccountPolicy;
use naw_core::error::AppError;
use naw_core::state::AppState;

/// The key of the account rules row in `install_settings`.
pub const ACCOUNTS_KEY: &str = "accounts";

/// The account rules in force: each field from the admin panel when it was
/// saved there, from the environment otherwise.
pub async fn accounts(state: &AppState) -> Result<AccountPolicy, AppError> {
    let stored = sqlx::query_scalar!(
        "SELECT value FROM install_settings WHERE key = $1",
        ACCOUNTS_KEY
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(merge(state.config.accounts, stored.as_ref()))
}

/// Overlays the stored fields on the defaults. A field that is missing or of
/// the wrong type keeps the default rather than failing the request.
fn merge(defaults: AccountPolicy, stored: Option<&serde_json::Value>) -> AccountPolicy {
    let Some(stored) = stored else {
        return defaults;
    };
    let int = |key: &str, fallback: i64| {
        stored
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(fallback)
    };
    AccountPolicy {
        rename_enabled: stored
            .get("rename_enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(defaults.rename_enabled),
        rename_cooldown_days: int("rename_cooldown_days", defaults.rename_cooldown_days),
        aliases_disabled: stored
            .get("aliases_disabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(defaults.aliases_disabled),
        alias_days: int("alias_days", defaults.alias_days),
        max_aliases: int("max_aliases", defaults.max_aliases),
    }
    .clamped()
}

/// Saves the account rules as the admin panel override.
pub async fn save_accounts(
    state: &AppState,
    policy: AccountPolicy,
    by: Option<uuid::Uuid>,
) -> Result<(), AppError> {
    let value = serde_json::to_value(policy.clamped()).map_err(|_| AppError::Internal)?;
    sqlx::query!(
        "INSERT INTO install_settings (key, value, updated_by) VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
                                         updated_at = now(),
                                         updated_by = EXCLUDED.updated_by",
        ACCOUNTS_KEY,
        value,
        by
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_fields_override_and_missing_ones_fall_back() {
        let defaults = AccountPolicy::default();
        assert_eq!(merge(defaults, None), defaults);
        let partial = serde_json::json!({ "alias_days": 90, "rename_enabled": false });
        let merged = merge(defaults, Some(&partial));
        assert_eq!(merged.alias_days, 90);
        assert!(!merged.rename_enabled);
        assert_eq!(merged.max_aliases, defaults.max_aliases);
        let junk = serde_json::json!({ "max_aliases": "lots", "alias_days": 99999 });
        let merged = merge(defaults, Some(&junk));
        assert_eq!(merged.max_aliases, defaults.max_aliases);
        assert_eq!(merged.alias_days, 3650, "clamped");
    }
}

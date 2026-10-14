//! `/system:ping`: how far the reader is from the wiki, and, when the admins
//! allow it, how far the wiki is from its own parts.
//!
//! The page measures the reader's side itself: a script times small
//! requests to `/health`, which touches nothing. The server's side is
//! `/system:ping?probe=parts`, which times one round trip to each part the
//! wiki runs on (the database, the cache, file storage and the diagram
//! worker when there is one) and answers in JSON. Those figures are kept for
//! a few seconds and shared by every reader, so asking for them often costs
//! the parts nothing more.
//!
//! Both are off until an admin turns them on, in `wikis.settings`:
//!
//! ```json
//! { "system": { "ping": { "page": true, "parts": true } } }
//! ```

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use naw_core::state::AppState;

/// How long one measurement of the parts is shared.
const PARTS_FRESH: Duration = Duration::from_secs(3);
/// How long a part may take before it counts as down.
const PART_TIMEOUT: Duration = Duration::from_secs(2);

/// What the admins turned on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// The page exists.
    pub page: bool,
    /// It also shows the server's own round trips.
    pub parts: bool,
}

pub fn flags(settings: &Value) -> Flags {
    let ping = settings.get("system").and_then(|s| s.get("ping"));
    let on = |key: &str| {
        ping.and_then(|p| p.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let page = on("page");
    Flags {
        page,
        // Without the page there is nowhere to show them.
        parts: page && on("parts"),
    }
}

/// The last measurement, and when it was taken.
static LAST: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

/// The server's round trip to each of its parts, in milliseconds, `null`
/// for a part that failed or took too long. Shared for `PARTS_FRESH`.
pub(crate) async fn parts(state: &AppState) -> Value {
    if let Some((at, value)) = LAST.lock().ok().and_then(|g| g.clone())
        && at.elapsed() < PARTS_FRESH
    {
        return value;
    }
    let (db, cache, storage, worker) = tokio::join!(
        timed(async {
            sqlx::query_scalar!("SELECT 1 AS one")
                .fetch_one(&state.db)
                .await
                .is_ok()
        }),
        timed(async {
            let Ok(mut conn) = state.valkey.get().await else {
                return false;
            };
            deadpool_redis::redis::cmd("PING")
                .query_async::<String>(&mut conn)
                .await
                .is_ok_and(|pong| pong == "PONG")
        }),
        timed(async { state.storage.ping().await.is_ok() }),
        async {
            match state.config.worker_url.as_deref() {
                Some(url) => Some(timed(worker_up(url)).await),
                None => None,
            }
        },
    );
    let mut list = vec![
        json!({ "id": "database", "ms": db }),
        json!({ "id": "cache", "ms": cache }),
        json!({ "id": "storage", "ms": storage }),
    ];
    if let Some(ms) = worker {
        list.push(json!({ "id": "worker", "ms": ms }));
    }
    let value = json!({ "parts": list });
    if let Ok(mut last) = LAST.lock() {
        *last = Some((Instant::now(), value.clone()));
    }
    value
}

/// Whether the diagram worker answers its health check.
async fn worker_up(url: &str) -> bool {
    static CLIENT: std::sync::LazyLock<Option<reqwest::Client>> = std::sync::LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(PART_TIMEOUT)
            .build()
            .ok()
    });
    let Some(client) = CLIENT.as_ref() else {
        return false;
    };
    client
        .get(format!("{}/health", url.trim_end_matches('/')))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// How long `check` took in milliseconds, to a tenth, or `None` when it
/// failed or ran out of time.
async fn timed(check: impl std::future::Future<Output = bool>) -> Option<f64> {
    let start = Instant::now();
    let ok = tokio::time::timeout(PART_TIMEOUT, check)
        .await
        .unwrap_or(false);
    ok.then(|| (start.elapsed().as_secs_f64() * 10_000.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_off_until_turned_on() {
        assert_eq!(flags(&json!({})), Flags::default());
        assert_eq!(
            flags(&json!({ "system": { "ping": { "page": "yes", "parts": true } } })),
            Flags::default(),
            "only a real true turns it on, and parts need the page"
        );
        assert_eq!(
            flags(&json!({ "system": { "ping": { "page": true } } })),
            Flags {
                page: true,
                parts: false
            }
        );
        assert_eq!(
            flags(&json!({ "system": { "ping": { "page": true, "parts": true } } })),
            Flags {
                page: true,
                parts: true
            }
        );
    }

    #[tokio::test]
    async fn a_check_is_timed_and_a_failure_has_no_time() {
        let ms = timed(async { true }).await.expect("a success has a time");
        assert!((0.0..1000.0).contains(&ms));
        assert_eq!(timed(async { false }).await, None);
    }
}

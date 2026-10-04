//! Alerts for the admins, through the wiki's bots (Telegram and Discord).
//!
//! Every minute the last minutes are checked against a few rules: a burst
//! of 5xx answers, and reader pages getting slow. A rule that starts
//! firing sends one message, repeats at most every [`REPEAT_MINUTES`] while
//! it lasts, and sends one more when it clears. An error the engine has not
//! logged in the last day is reported as it happens, a few at a time.
//!
//! Recipients are the install's staff and every wiki's owners and admins
//! who linked a bot and did not turn these off in their settings.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use naw_core::state::AppState;

use crate::bots;

/// Minutes a rule looks back over.
const WINDOW_MINUTES: usize = 5;

/// A firing rule repeats no sooner than this.
const REPEAT_MINUTES: i64 = 30;

/// 5xx answers in the window that start the error alert, and the share of
/// all requests they must also reach.
const ERRORS_MIN: u32 = 5;
const ERRORS_SHARE: f64 = 0.01;

/// Reader requests in the window before latency counts, and the p95 that
/// starts the slowness alert, in milliseconds.
const SLOW_MIN_REQUESTS: u32 = 30;
const SLOW_P95_MS: f64 = 2000.0;

/// New error kinds reported in one message, at most.
const NEW_ERRORS_SHOWN: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Rule {
    Errors,
    Slow,
}

impl Rule {
    fn key(self) -> &'static str {
        match self {
            Self::Errors => "errors",
            Self::Slow => "slow",
        }
    }
}

struct Firing {
    since: chrono::DateTime<chrono::Utc>,
    last_sent: chrono::DateTime<chrono::Utc>,
}

static FIRING: Mutex<Option<HashMap<Rule, Firing>>> = Mutex::new(None);

/// What a rule found, when it fires.
struct Finding {
    rule: Rule,
    /// Arguments for its message.
    args: Vec<(&'static str, String)>,
}

/// The rules over the last [`WINDOW_MINUTES`].
fn check() -> Vec<Finding> {
    let mut found = Vec::new();
    let (all, minutes) = crate::metrics::recent(WINDOW_MINUTES, None);
    if minutes == 0 {
        return found;
    }
    if all.server_errors >= ERRORS_MIN
        && f64::from(all.server_errors) >= ERRORS_SHARE * f64::from(all.requests.max(1))
    {
        let mut groups: Vec<(&'static str, u32)> = crate::metrics::recent_by_group(WINDOW_MINUTES)
            .into_iter()
            .filter(|(_, agg)| agg.server_errors > 0)
            .map(|(group, agg)| (group, agg.server_errors))
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.1));
        let top = groups
            .iter()
            .take(3)
            .map(|(group, n)| format!("{group}: {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        found.push(Finding {
            rule: Rule::Errors,
            args: vec![
                ("count", all.server_errors.to_string()),
                ("total", all.requests.to_string()),
                (
                    "share",
                    format!(
                        "{:.1}",
                        100.0 * f64::from(all.server_errors) / f64::from(all.requests.max(1))
                    ),
                ),
                ("minutes", WINDOW_MINUTES.to_string()),
                ("where", top),
            ],
        });
    }
    let (readers, _) =
        crate::metrics::recent(WINDOW_MINUTES, Some(crate::metrics::reader_groups()));
    if readers.requests >= SLOW_MIN_REQUESTS {
        let p95 = readers.percentile(0.95);
        if p95 > SLOW_P95_MS {
            found.push(Finding {
                rule: Rule::Slow,
                args: vec![
                    ("p50", format!("{:.0}", readers.percentile(0.5))),
                    ("p95", format!("{p95:.0}")),
                    ("p99", format!("{:.0}", readers.percentile(0.99))),
                    ("minutes", WINDOW_MINUTES.to_string()),
                ],
            });
        }
    }
    found
}

/// What to send now: firings that are new or due a repeat, and rules that
/// cleared. Updates the remembered state.
fn decide(
    found: &[Finding],
    now: chrono::DateTime<chrono::Utc>,
    state: &mut HashMap<Rule, Firing>,
) -> (Vec<usize>, Vec<(Rule, i64)>) {
    let mut send = Vec::new();
    for (i, finding) in found.iter().enumerate() {
        match state.get_mut(&finding.rule) {
            Some(firing) => {
                if (now - firing.last_sent).num_minutes() >= REPEAT_MINUTES {
                    firing.last_sent = now;
                    send.push(i);
                }
            }
            None => {
                state.insert(
                    finding.rule,
                    Firing {
                        since: now,
                        last_sent: now,
                    },
                );
                send.push(i);
            }
        }
    }
    let still: Vec<Rule> = found.iter().map(|f| f.rule).collect();
    let cleared: Vec<(Rule, i64)> = state
        .iter()
        .filter(|(rule, _)| !still.contains(rule))
        .map(|(rule, firing)| (*rule, (now - firing.since).num_minutes().max(1)))
        .collect();
    for (rule, _) in &cleared {
        state.remove(rule);
    }
    (send, cleared)
}

/// Runs the rules and sends what is due. Called every minute.
pub(crate) async fn evaluate(state: &AppState) {
    if !bots::any() {
        return;
    }
    let found = check();
    let (send, cleared) = {
        let Ok(mut guard) = FIRING.lock() else {
            return;
        };
        let firing = guard.get_or_insert_with(HashMap::new);
        decide(&found, chrono::Utc::now(), firing)
    };
    for i in send {
        let finding = &found[i];
        let args: Vec<(&str, &str)> = finding.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        broadcast(state, &format!("ops_{}", finding.rule.key()), &args).await;
    }
    for (rule, minutes) in cleared {
        broadcast(
            state,
            &format!("ops_{}_cleared", rule.key()),
            &[("minutes", &minutes.to_string())],
        )
        .await;
    }
}

/// Minutes between two messages about new errors: a storm of different
/// errors is one message, and the page has the rest.
const NEW_ERRORS_PAUSE_MINUTES: i64 = 5;

static LAST_NEW_ERRORS: Mutex<Option<chrono::DateTime<chrono::Utc>>> = Mutex::new(None);

/// Errors seen for the first time in a day, as they are stored.
pub(crate) fn new_errors(state: &AppState, messages: Vec<String>) {
    if !bots::any() {
        return;
    }
    let now = chrono::Utc::now();
    {
        let Ok(mut last) = LAST_NEW_ERRORS.lock() else {
            return;
        };
        if last.is_some_and(|at| (now - at).num_minutes() < NEW_ERRORS_PAUSE_MINUTES) {
            return;
        }
        *last = Some(now);
    }
    let state = state.clone();
    tokio::spawn(async move {
        let shown: Vec<String> = messages
            .iter()
            .take(NEW_ERRORS_SHOWN)
            .map(|m| format!("- {}", m.chars().take(300).collect::<String>()))
            .collect();
        let more = messages.len().saturating_sub(NEW_ERRORS_SHOWN);
        let list = if more > 0 {
            format!("{}\n+{more}", shown.join("\n"))
        } else {
            shown.join("\n")
        };
        // The list is an argument, so it is escaped for each platform.
        broadcast(
            &state,
            "ops_new_errors",
            &[("count", &messages.len().to_string()), ("list", &list)],
        )
        .await;
    });
}

/// The admins who get alerts and have a bot to get them by.
async fn recipients(state: &AppState) -> Vec<uuid::Uuid> {
    sqlx::query_scalar!(
        r#"SELECT u.id FROM users u
           WHERE (u.global_role IN ('root', 'staff')
                  OR EXISTS (SELECT 1 FROM wiki_memberships m
                             WHERE m.user_id = u.id AND m.role IN ('owner', 'admin')))
             AND COALESCE((u.settings->'telegram'->>'ops')::boolean, true)
             AND (EXISTS (SELECT 1 FROM telegram_links tl WHERE tl.user_id = u.id)
                  OR EXISTS (SELECT 1 FROM discord_links dl WHERE dl.user_id = u.id))"#
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
}

/// Sends bot text `key` to every recipient on every chat they linked, in
/// their language, with a button to the monitoring page.
async fn broadcast(state: &AppState, key: &str, args: &[(&str, &str)]) {
    let (wiki, origin) = bots::site(state).await;
    let url = format!("{origin}/admin/monitoring");
    let mut full: Vec<(&str, &str)> = vec![("wiki", &wiki)];
    full.extend_from_slice(args);
    for user_id in recipients(state).await {
        bots::send_to_user(
            state,
            user_id,
            "ops_alert",
            key,
            &full,
            Some(("ops_open", &url)),
        )
        .await;
    }
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: None,
            action: "ops.alert",
            entity_type: "install",
            entity_id: None,
            meta: json!({ "alert": key }),
        },
    )
    .await;
}

/// The rules currently firing, for the monitoring page: key and minutes.
pub(crate) fn firing_now() -> Vec<(&'static str, i64)> {
    let now = chrono::Utc::now();
    FIRING
        .lock()
        .ok()
        .and_then(|guard| {
            guard.as_ref().map(|map| {
                map.iter()
                    .map(|(rule, f)| (rule.key(), (now - f.since).num_minutes()))
                    .collect()
            })
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(rule: Rule) -> Finding {
        Finding {
            rule,
            args: Vec::new(),
        }
    }

    #[test]
    fn a_rule_alerts_once_repeats_late_and_says_when_it_clears() {
        let mut state = HashMap::new();
        let t0 = chrono::Utc::now();
        let (send, cleared) = decide(&[finding(Rule::Errors)], t0, &mut state);
        assert_eq!((send.len(), cleared.len()), (1, 0), "first time: send");
        let (send, _) = decide(
            &[finding(Rule::Errors)],
            t0 + chrono::Duration::minutes(5),
            &mut state,
        );
        assert!(send.is_empty(), "still firing: quiet");
        let (send, _) = decide(
            &[finding(Rule::Errors)],
            t0 + chrono::Duration::minutes(REPEAT_MINUTES + 1),
            &mut state,
        );
        assert_eq!(send.len(), 1, "a repeat once the pause is over");
        let (send, cleared) = decide(&[], t0 + chrono::Duration::minutes(40), &mut state);
        assert!(send.is_empty());
        assert_eq!(cleared.len(), 1, "cleared: one message");
        assert!(state.is_empty());
        let (_, cleared) = decide(&[], t0 + chrono::Duration::minutes(41), &mut state);
        assert!(cleared.is_empty(), "and only one");
    }
}

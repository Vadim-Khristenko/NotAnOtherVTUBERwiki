//! What the server did, for the admins: requests, latency percentiles,
//! load and errors, and the alerts that tell them when something is wrong.
//!
//! Every request is counted in memory, per minute and route group, into a
//! latency histogram with fixed bounds ([`BOUNDS_MS`]). A finished minute is
//! written to `metrics_minutes`, one row per group, so percentiles over an
//! hour or a week come from merged histograms rather than averaged averages.
//! The most requests in flight at once goes to `metrics_load`.
//!
//! Errors come from two places: every 5xx answer (with its request id), and
//! every event the engine logs at error level (captured by
//! `naw_core::logging`). Both land in `error_events` with a fingerprint, so
//! the page groups them and an alert can tell a new problem from an old one.
//!
//! Alerts go to the admins who linked Telegram (see `telegram.rs`), with a
//! pause between repeats and a message when the problem clears. A process
//! that is down cannot report itself; that needs a check from outside.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use uuid::Uuid;

use naw_core::state::AppState;

/// Upper bounds of the latency buckets, in milliseconds. A last bucket holds
/// everything slower.
pub(crate) const BOUNDS_MS: [u32; 23] = [
    1, 2, 3, 5, 7, 10, 15, 20, 30, 50, 75, 100, 150, 200, 300, 500, 750, 1000, 1500, 2000, 3000,
    5000, 10000,
];

pub(crate) const BUCKETS: usize = BOUNDS_MS.len() + 1;

/// Minutes kept in memory for the alerts.
const RECENT_MINUTES: usize = 90;

/// Days of minutes and of errors kept in the database.
const KEEP_METRICS_DAYS: i32 = 30;
const KEEP_ERRORS_DAYS: i32 = 14;

/// One minute of one route group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Agg {
    pub requests: u32,
    pub server_errors: u32,
    pub client_errors: u32,
    pub hist: [u32; BUCKETS],
    pub sum_ms: u64,
    pub max_ms: u32,
}

impl Agg {
    fn add(&mut self, status: u16, ms: u32) {
        self.requests += 1;
        if status >= 500 {
            self.server_errors += 1;
        } else if status >= 400 {
            self.client_errors += 1;
        }
        self.hist[bucket(ms)] += 1;
        self.sum_ms += u64::from(ms);
        self.max_ms = self.max_ms.max(ms);
    }

    pub(crate) fn merge(&mut self, other: &Agg) {
        self.requests += other.requests;
        self.server_errors += other.server_errors;
        self.client_errors += other.client_errors;
        for (mine, theirs) in self.hist.iter_mut().zip(other.hist.iter()) {
            *mine += theirs;
        }
        self.sum_ms += other.sum_ms;
        self.max_ms = self.max_ms.max(other.max_ms);
    }

    /// The latency below which `q` (0 to 1) of the requests finished, read
    /// off the histogram and interpolated inside its bucket.
    pub(crate) fn percentile(&self, q: f64) -> f64 {
        let total: u64 = self.hist.iter().map(|&n| u64::from(n)).sum();
        if total == 0 {
            return 0.0;
        }
        let rank = (q.clamp(0.0, 1.0) * total as f64).max(1.0);
        let mut seen = 0f64;
        for (i, &n) in self.hist.iter().enumerate() {
            if n == 0 {
                continue;
            }
            let next = seen + f64::from(n);
            if next >= rank {
                let low = if i == 0 {
                    0.0
                } else {
                    f64::from(BOUNDS_MS[i - 1])
                };
                let high = if i < BOUNDS_MS.len() {
                    f64::from(BOUNDS_MS[i])
                } else {
                    f64::from(self.max_ms).max(low)
                };
                let within = (rank - seen) / f64::from(n);
                return (low + (high - low) * within).min(f64::from(self.max_ms.max(1)));
            }
            seen = next;
        }
        f64::from(self.max_ms)
    }
}

fn bucket(ms: u32) -> usize {
    BOUNDS_MS
        .iter()
        .position(|&bound| ms <= bound)
        .unwrap_or(BOUNDS_MS.len())
}

/// The group a request is counted under, from its method and path.
pub(crate) fn group(method: &str, path: &str) -> &'static str {
    let p = path;
    if p == "/health" || p == "/ready" {
        return "health";
    }
    if p.starts_with("/skin/")
        || p.starts_with("/favicon")
        || p.starts_with("/apple-touch-icon")
        || p.starts_with("/web-app-manifest")
        || p == "/site.webmanifest"
        || p == "/emote-cache.js"
    {
        return "static";
    }
    if p == "/sitemap.xml" || p == "/robots.txt" {
        return "seo";
    }
    if p.starts_with("/media/") || p == "/media" {
        return "media";
    }
    if p.starts_with("/admin") {
        return "admin";
    }
    if p.starts_with("/auth/")
        || p.starts_with("/login")
        || p == "/logout"
        || p.starts_with("/password/")
        || p.starts_with("/settings")
    {
        return "account";
    }
    if p.starts_with("/search") {
        return "search";
    }
    if p.starts_with("/system") {
        return "system";
    }
    if p == "/preview" {
        return "preview";
    }
    if p.starts_with("/notifications") || p.starts_with("/watchlist") || p.starts_with("/drafts") {
        return "personal";
    }
    if p.ends_with("/edit") || p == "/new" {
        return "edit";
    }
    if p.ends_with("/history") {
        return "history";
    }
    if p.ends_with("/diff") || p.contains("/rev/") {
        return "diff";
    }
    if p.contains("category:")
        || p.contains("%D0%BA%D0%B0%D1%82%D0%B5%D0%B3%D0%BE%D1%80%D0%B8%D1%8F:")
    {
        return "category";
    }
    if method == "GET" || method == "HEAD" {
        "page"
    } else {
        "action"
    }
}

/// Groups whose slowness readers feel: the latency alert watches these.
const READER_GROUPS: &[&str] = &["page", "category", "system", "search"];

struct Minute {
    start: i64,
    groups: BTreeMap<&'static str, Agg>,
    peak_in_flight: i64,
}

impl Minute {
    fn new(start: i64) -> Self {
        Self {
            start,
            groups: BTreeMap::new(),
            peak_in_flight: IN_FLIGHT.load(Ordering::Relaxed),
        }
    }
}

/// A minute that is over.
#[derive(Clone)]
pub(crate) struct Finished {
    pub start: i64,
    pub groups: BTreeMap<&'static str, Agg>,
    pub peak_in_flight: i64,
}

static IN_FLIGHT: AtomicI64 = AtomicI64::new(0);
static CURRENT: Mutex<Option<Minute>> = Mutex::new(None);
/// Finished minutes not written yet.
static UNWRITTEN: Mutex<Vec<Finished>> = Mutex::new(Vec::new());
/// The last [`RECENT_MINUTES`] finished minutes, for the alerts.
static RECENT: Mutex<VecDeque<Finished>> = Mutex::new(VecDeque::new());

fn now_minute() -> i64 {
    chrono::Utc::now().timestamp().div_euclid(60)
}

/// Closes the current minute when the clock has moved past it.
fn roll(current: &mut Option<Minute>, now: i64) {
    let Some(minute) = current.as_ref() else {
        *current = Some(Minute::new(now));
        return;
    };
    if minute.start == now {
        return;
    }
    if let Some(done) = current.take() {
        let finished = Finished {
            start: done.start,
            groups: done.groups,
            peak_in_flight: done.peak_in_flight,
        };
        if let Ok(mut unwritten) = UNWRITTEN.lock() {
            unwritten.push(finished.clone());
        }
        if let Ok(mut recent) = RECENT.lock() {
            recent.push_back(finished);
            while recent.len() > RECENT_MINUTES {
                recent.pop_front();
            }
        }
    }
    *current = Some(Minute::new(now));
}

/// Counts a request as started; dropping the guard counts it as done.
pub(crate) struct InFlight;

pub(crate) fn begin() -> InFlight {
    let now = IN_FLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
    if let Ok(mut current) = CURRENT.lock() {
        roll(&mut current, now_minute());
        if let Some(minute) = current.as_mut() {
            minute.peak_in_flight = minute.peak_in_flight.max(now);
        }
    }
    InFlight
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Requests being served right now.
pub(crate) fn in_flight() -> i64 {
    IN_FLIGHT.load(Ordering::Relaxed).max(0)
}

/// Counts one finished request.
pub(crate) fn record(method: &str, path: &str, status: u16, elapsed_ms: u128) {
    let ms = u32::try_from(elapsed_ms).unwrap_or(u32::MAX);
    let group = group(method, path);
    if let Ok(mut current) = CURRENT.lock() {
        roll(&mut current, now_minute());
        if let Some(minute) = current.as_mut() {
            minute.groups.entry(group).or_default().add(status, ms);
        }
    }
}

/// A 5xx answer, kept with its request id for the error list.
pub(crate) fn server_error(request_id: &str, method: &str, path: &str, status: u16) {
    if let Ok(mut pending) = PENDING_ERRORS.lock()
        && pending.len() < 500
    {
        {
            pending.push(ErrorEvent {
                source: "response",
                fingerprint: format!("{status} {}", group(method, path)),
                message: format!("{status} on {method} {}", group(method, path)),
                request_id: Some(request_id.to_string()),
                method: Some(method.to_string()),
                path: Some(path.chars().take(300).collect()),
                status: Some(i32::from(status)),
            });
        }
    }
}

struct ErrorEvent {
    source: &'static str,
    fingerprint: String,
    message: String,
    request_id: Option<String>,
    method: Option<String>,
    path: Option<String>,
    status: Option<i32>,
}

static PENDING_ERRORS: Mutex<Vec<ErrorEvent>> = Mutex::new(Vec::new());

/// A logged error's fingerprint: where it came from and what it said, with
/// numbers and ids taken out so one problem stays one group.
pub(crate) fn fingerprint(target: &str, message: &str) -> String {
    let mut out = String::with_capacity(target.len() + message.len() + 1);
    out.push_str(target);
    out.push(' ');
    let mut last_digit = false;
    for c in message.chars().take(200) {
        if c.is_ascii_digit() {
            if !last_digit {
                out.push('#');
            }
            last_digit = true;
        } else {
            out.push(c);
            last_digit = false;
        }
    }
    out
}

/// Moves finished minutes and new errors into the database, and drops what
/// is past keeping. Called every few seconds and before the page reads.
pub(crate) async fn flush(state: &AppState) {
    if let Ok(mut current) = CURRENT.lock() {
        roll(&mut current, now_minute());
    }
    let minutes: Vec<Finished> = UNWRITTEN
        .lock()
        .map(|mut u| std::mem::take(&mut *u))
        .unwrap_or_default();
    for minute in &minutes {
        if let Err(err) = write_minute(&state.db, minute).await {
            tracing::warn!(error = %err, "could not write a metrics minute");
        }
    }
    let mut events: Vec<ErrorEvent> = PENDING_ERRORS
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default();
    for captured in naw_core::logging::take_captured() {
        // A 5xx is logged as "request failed" too; it is already an event of
        // its own, with its status and route group.
        if captured.target.ends_with("::observe") && captured.message == "request failed" {
            continue;
        }
        let message = if captured.fields.is_empty() {
            captured.message.clone()
        } else {
            format!("{} ({})", captured.message, captured.fields)
        };
        events.push(ErrorEvent {
            source: "log",
            fingerprint: fingerprint(&captured.target, &captured.message),
            message: format!("{}: {message}", captured.target),
            request_id: captured.request_id,
            method: None,
            path: None,
            status: None,
        });
    }
    let mut new_problems = Vec::new();
    for event in events {
        let seen_before = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM error_events
                 WHERE fingerprint = $1 AND at > now() - interval '24 hours') AS "seen!""#,
            event.fingerprint
        )
        .fetch_one(&state.db)
        .await
        .unwrap_or(true);
        let stored = sqlx::query!(
            "INSERT INTO error_events (id, source, fingerprint, message, request_id, method, path, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            Uuid::new_v4(),
            event.source,
            event.fingerprint,
            event.message,
            event.request_id,
            event.method,
            event.path,
            event.status
        )
        .execute(&state.db)
        .await;
        if let Err(err) = stored {
            tracing::warn!(error = %err, "could not store an error event");
            continue;
        }
        if !seen_before && event.source == "log" {
            new_problems.push(event.message);
        }
    }
    if !new_problems.is_empty() {
        crate::alerts::new_errors(state, new_problems);
    }
}

async fn write_minute(db: &sqlx::PgPool, minute: &Finished) -> Result<(), sqlx::Error> {
    let at = chrono::DateTime::from_timestamp(minute.start * 60, 0).unwrap_or_default();
    for (group, agg) in &minute.groups {
        let hist: Vec<i32> = agg
            .hist
            .iter()
            .map(|&n| i32::try_from(n).unwrap_or(i32::MAX))
            .collect();
        sqlx::query!(
            "INSERT INTO metrics_minutes
               (minute, route, requests, server_errors, client_errors, hist, sum_ms, max_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (minute, route) DO UPDATE SET
               requests = metrics_minutes.requests + EXCLUDED.requests,
               server_errors = metrics_minutes.server_errors + EXCLUDED.server_errors,
               client_errors = metrics_minutes.client_errors + EXCLUDED.client_errors,
               sum_ms = metrics_minutes.sum_ms + EXCLUDED.sum_ms,
               max_ms = GREATEST(metrics_minutes.max_ms, EXCLUDED.max_ms),
               hist = ARRAY(SELECT COALESCE(a, 0) + COALESCE(b, 0)
                            FROM unnest(metrics_minutes.hist, EXCLUDED.hist) AS t(a, b))",
            at,
            *group,
            i32::try_from(agg.requests).unwrap_or(i32::MAX),
            i32::try_from(agg.server_errors).unwrap_or(i32::MAX),
            i32::try_from(agg.client_errors).unwrap_or(i32::MAX),
            &hist,
            i64::try_from(agg.sum_ms).unwrap_or(i64::MAX),
            i32::try_from(agg.max_ms).unwrap_or(i32::MAX)
        )
        .execute(db)
        .await?;
    }
    sqlx::query!(
        "INSERT INTO metrics_load (minute, peak_in_flight) VALUES ($1, $2)
         ON CONFLICT (minute) DO UPDATE SET peak_in_flight = GREATEST(metrics_load.peak_in_flight, EXCLUDED.peak_in_flight)",
        at,
        i32::try_from(minute.peak_in_flight).unwrap_or(i32::MAX)
    )
    .execute(db)
    .await?;
    Ok(())
}

async fn prune(db: &sqlx::PgPool) {
    let _ = sqlx::query!(
        "DELETE FROM metrics_minutes WHERE minute < now() - make_interval(days => $1)",
        KEEP_METRICS_DAYS
    )
    .execute(db)
    .await;
    let _ = sqlx::query!(
        "DELETE FROM metrics_load WHERE minute < now() - make_interval(days => $1)",
        KEEP_METRICS_DAYS
    )
    .execute(db)
    .await;
    let _ = sqlx::query!(
        "DELETE FROM error_events WHERE at < now() - make_interval(days => $1)",
        KEEP_ERRORS_DAYS
    )
    .execute(db)
    .await;
}

/// The last `minutes` finished minutes, all groups or only `groups`, merged.
pub(crate) fn recent(minutes: usize, groups: Option<&[&str]>) -> (Agg, usize) {
    let Ok(recent) = RECENT.lock() else {
        return (Agg::default(), 0);
    };
    let mut total = Agg::default();
    let taken = recent.iter().rev().take(minutes);
    let mut count = 0;
    for minute in taken {
        count += 1;
        for (group, agg) in &minute.groups {
            if groups.is_none_or(|wanted| wanted.contains(group)) {
                total.merge(agg);
            }
        }
    }
    (total, count)
}

/// Requests per group in the last `minutes`, for an alert's detail.
pub(crate) fn recent_by_group(minutes: usize) -> HashMap<&'static str, Agg> {
    let mut out: HashMap<&'static str, Agg> = HashMap::new();
    if let Ok(recent) = RECENT.lock() {
        for minute in recent.iter().rev().take(minutes) {
            for (group, agg) in &minute.groups {
                out.entry(group).or_default().merge(agg);
            }
        }
    }
    out
}

/// Latency groups that readers feel, for the alert.
pub(crate) fn reader_groups() -> &'static [&'static str] {
    READER_GROUPS
}

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Seconds since the process started serving.
pub(crate) fn uptime_secs() -> u64 {
    STARTED
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs()
}

/// The work beside the server: writing minutes, pruning, and the alerts.
pub fn start(state: AppState) {
    STARTED.get_or_init(std::time::Instant::now);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            flush(&state).await;
            ticks += 1;
            // Every minute, a look at the last minutes for the alerts.
            if ticks.is_multiple_of(4) {
                crate::alerts::evaluate(&state).await;
            }
            // Every hour, what is past keeping goes.
            if ticks % 240 == 1 {
                prune(&state.db).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg_of(samples: &[u32]) -> Agg {
        let mut agg = Agg::default();
        for &ms in samples {
            agg.add(200, ms);
        }
        agg
    }

    #[test]
    fn percentiles_come_from_the_histogram() {
        let samples: Vec<u32> = (1..=100).collect();
        let agg = agg_of(&samples);
        let p50 = agg.percentile(0.5);
        let p99 = agg.percentile(0.99);
        assert!((40.0..=60.0).contains(&p50), "p50 {p50}");
        assert!((90.0..=100.0).contains(&p99), "p99 {p99}");
        assert!(p99 <= f64::from(agg.max_ms));
        assert_eq!(
            Agg::default().percentile(0.5),
            0.0,
            "no requests, no latency"
        );
    }

    #[test]
    fn a_slow_tail_shows_in_p99_not_p50() {
        let mut samples = vec![5u32; 990];
        samples.extend(std::iter::repeat_n(4000u32, 10));
        let agg = agg_of(&samples);
        assert!(agg.percentile(0.5) <= 7.0);
        assert!(agg.percentile(0.995) >= 3000.0);
    }

    #[test]
    fn merged_minutes_add_up() {
        let mut a = agg_of(&[10, 20]);
        let mut b = agg_of(&[30]);
        b.add(503, 5);
        a.merge(&b);
        assert_eq!((a.requests, a.server_errors, a.max_ms), (4, 1, 30));
        assert_eq!(a.hist.iter().sum::<u32>(), 4);
    }

    #[test]
    fn requests_fall_into_groups() {
        assert_eq!(group("GET", "/filian"), "page");
        assert_eq!(group("GET", "/filian/edit"), "edit");
        assert_eq!(group("POST", "/filian/edit"), "edit");
        assert_eq!(group("GET", "/filian/history"), "history");
        assert_eq!(group("GET", "/category:vtubers"), "category");
        assert_eq!(group("GET", "/skin/app.css"), "static");
        assert_eq!(group("GET", "/admin/monitoring"), "admin");
        assert_eq!(group("POST", "/password/forgot"), "account");
        assert_eq!(group("POST", "/filian/watch"), "action");
    }

    #[test]
    fn a_fingerprint_ignores_numbers() {
        assert_eq!(
            fingerprint("naw_web::pages", "query took 1234 ms on row 77"),
            fingerprint("naw_web::pages", "query took 98 ms on row 3")
        );
        assert_ne!(
            fingerprint("naw_web::pages", "template error"),
            fingerprint("naw_web::media", "template error")
        );
    }
}

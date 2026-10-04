//! `/admin/monitoring`: the server's last hour, day or week at a glance.
//!
//! Percentiles over the whole range and per route group, the busiest minute
//! and the most requests at once, two charts (load with errors, latency
//! percentiles) drawn as plain SVG, the errors grouped by fingerprint, and
//! the process as it is right now. No script is needed to read any of it.

use std::collections::BTreeMap;

use axum::extract::{Extension, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::metrics::{Agg, BUCKETS};
use crate::perm::Capability;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Range {
    Hour,
    Day,
    Week,
}

impl Range {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("24h") => Self::Day,
            Some("7d") => Self::Week,
            _ => Self::Hour,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    fn minutes(self) -> i64 {
        match self {
            Self::Hour => 60,
            Self::Day => 24 * 60,
            Self::Week => 7 * 24 * 60,
        }
    }

    /// Minutes per point on the charts.
    fn step(self) -> i64 {
        match self {
            Self::Hour => 1,
            Self::Day => 15,
            Self::Week => 120,
        }
    }
}

#[derive(Deserialize)]
pub struct MonitoringQuery {
    #[serde(default)]
    range: Option<String>,
}

const CHART_W: f64 = 720.0;
const CHART_H: f64 = 160.0;

/// 1, 2 or 5 times a power of ten, at least `v`: a round top for an axis.
fn nice(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let power = 10f64.powf(v.log10().floor());
    for step in [1.0, 2.0, 5.0, 10.0] {
        if step * power >= v {
            return step * power;
        }
    }
    10.0 * power
}

/// Milliseconds as people read them.
fn ms(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.2} s", v / 1000.0)
    } else if v >= 10.0 {
        format!("{v:.0} ms")
    } else {
        format!("{v:.1} ms")
    }
}

fn percentiles(agg: &Agg) -> minijinja::Value {
    minijinja::context! {
        p25 => ms(agg.percentile(0.25)),
        p50 => ms(agg.percentile(0.50)),
        p75 => ms(agg.percentile(0.75)),
        p90 => ms(agg.percentile(0.90)),
        p95 => ms(agg.percentile(0.95)),
        p99 => ms(agg.percentile(0.99)),
        max => ms(f64::from(agg.max_ms)),
        avg => ms(if agg.requests > 0 { agg.sum_ms as f64 / f64::from(agg.requests) } else { 0.0 }),
    }
}

fn agg_from_row(
    requests: i32,
    server: i32,
    client: i32,
    hist: &[i32],
    sum_ms: i64,
    max_ms: i32,
) -> Agg {
    let mut agg = Agg {
        requests: requests.max(0) as u32,
        server_errors: server.max(0) as u32,
        client_errors: client.max(0) as u32,
        sum_ms: sum_ms.max(0) as u64,
        max_ms: max_ms.max(0) as u32,
        ..Agg::default()
    };
    for (slot, &n) in agg.hist.iter_mut().zip(hist.iter().take(BUCKETS)) {
        *slot = n.max(0) as u32;
    }
    agg
}

/// The resident memory of this process, in MiB, where the system tells.
fn resident_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

/// GET /admin/monitoring
pub async fn page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Query(query): Query<MonitoringQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(
        crate::admin::gate_for(&state, &headers, user.as_ref(), Capability::AdminPanel).await
    );
    // What the last seconds counted, before reading.
    crate::metrics::flush(&state).await;
    let range = Range::parse(query.range.as_deref());
    let now = chrono::Utc::now();
    let step = range.step();
    let points = (range.minutes() / step) as usize;
    // The window ends at the current step, so the newest point is live.
    let end_minute = now.timestamp().div_euclid(60).div_euclid(step) * step + step;
    let start_minute = end_minute - range.minutes();
    let start = chrono::DateTime::from_timestamp(start_minute * 60, 0).unwrap_or(now);

    let rows = sqlx::query!(
        "SELECT minute, route, requests, server_errors, client_errors, hist, sum_ms, max_ms
         FROM metrics_minutes WHERE minute >= $1 ORDER BY minute",
        start
    )
    .fetch_all(&state.db)
    .await?;
    let load = sqlx::query!(
        "SELECT minute, peak_in_flight FROM metrics_load WHERE minute >= $1",
        start
    )
    .fetch_all(&state.db)
    .await?;

    let mut buckets = vec![Agg::default(); points];
    let mut peaks = vec![0i32; points];
    let mut per_minute: BTreeMap<i64, u32> = BTreeMap::new();
    let mut per_route: BTreeMap<String, Agg> = BTreeMap::new();
    let mut total = Agg::default();
    for row in &rows {
        let agg = agg_from_row(
            row.requests,
            row.server_errors,
            row.client_errors,
            &row.hist,
            row.sum_ms,
            row.max_ms,
        );
        let minute = row.minute.timestamp().div_euclid(60);
        let index = ((minute - start_minute) / step).clamp(0, points as i64 - 1) as usize;
        buckets[index].merge(&agg);
        *per_minute.entry(minute).or_default() += agg.requests;
        per_route.entry(row.route.clone()).or_default().merge(&agg);
        total.merge(&agg);
    }
    let mut peak_in_flight = 0;
    for row in &load {
        let minute = row.minute.timestamp().div_euclid(60);
        let index = ((minute - start_minute) / step).clamp(0, points as i64 - 1) as usize;
        peaks[index] = peaks[index].max(row.peak_in_flight);
        peak_in_flight = peak_in_flight.max(row.peak_in_flight);
    }
    let (peak_minute, peak_requests) = per_minute
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(m, n)| (*m, *n))
        .unwrap_or((0, 0));
    let active_minutes = per_minute.len().max(1) as f64;

    // Load chart: requests per point as bars, 5xx over them.
    let top_requests = nice(
        buckets
            .iter()
            .map(|b| f64::from(b.requests))
            .fold(0.0, f64::max),
    );
    let bar_w = CHART_W / points as f64;
    let label_of = |index: usize| -> String {
        let minute = start_minute + index as i64 * step;
        let at = chrono::DateTime::from_timestamp(minute * 60, 0).unwrap_or(now);
        match range {
            Range::Hour | Range::Day => at.format("%H:%M").to_string(),
            Range::Week => at.format("%d.%m %H:00").to_string(),
        }
    };
    let bars: Vec<minijinja::Value> = buckets
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let h = CHART_H * f64::from(b.requests) / top_requests;
            let eh = CHART_H * f64::from(b.server_errors) / top_requests;
            minijinja::context! {
                x => format!("{:.2}", i as f64 * bar_w + bar_w * 0.1),
                w => format!("{:.2}", (bar_w * 0.8).max(0.6)),
                y => format!("{:.2}", CHART_H - h),
                h => format!("{:.2}", h),
                ey => format!("{:.2}", CHART_H - eh),
                eh => format!("{:.2}", if b.server_errors > 0 { eh.max(1.5) } else { 0.0 }),
                title => format!(
                    "{}: {} · 5xx {} · 4xx {} · p95 {} · {} {}",
                    label_of(i), b.requests, b.server_errors, b.client_errors,
                    ms(b.percentile(0.95)), ctx.t("monitoring.in_flight_short"), peaks[i]
                ),
            }
        })
        .collect();

    // Latency chart: p50, p95 and p99 per point, where there were requests.
    let top_latency = nice(
        buckets
            .iter()
            .filter(|b| b.requests > 0)
            .map(|b| b.percentile(0.99))
            .fold(0.0, f64::max),
    );
    let points_of = |q: f64| -> Vec<(f64, f64)> {
        buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| b.requests > 0)
            .map(|(i, b)| {
                let x = i as f64 * bar_w + bar_w / 2.0;
                let y = CHART_H - CHART_H * b.percentile(q) / top_latency;
                (x, y)
            })
            .collect()
    };
    let line = |q: f64| -> String {
        points_of(q)
            .iter()
            .map(|(x, y)| format!("{x:.1},{y:.1}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // A point of its own, so one busy minute still shows.
    let dots = |q: f64| -> Vec<minijinja::Value> {
        points_of(q)
            .iter()
            .map(|(x, y)| minijinja::context! { x => format!("{x:.1}"), y => format!("{y:.1}") })
            .collect()
    };

    let mut routes: Vec<(String, Agg)> = per_route.into_iter().collect();
    routes.sort_by_key(|r| std::cmp::Reverse(r.1.requests));
    let route_rows: Vec<minijinja::Value> = routes
        .iter()
        .map(|(name, agg)| {
            minijinja::context! {
                name => name,
                requests => agg.requests,
                share => format!("{:.1}", 100.0 * f64::from(agg.requests) / f64::from(total.requests.max(1))),
                server_errors => agg.server_errors,
                client_errors => agg.client_errors,
                p => percentiles(agg),
                slow => agg.percentile(0.95) > 1000.0,
            }
        })
        .collect();

    let errors = sqlx::query!(
        r#"SELECT fingerprint, source, count(*) AS "count!", max(at) AS "last!",
                  (array_agg(message ORDER BY at DESC))[1] AS "message!",
                  (array_agg(request_id ORDER BY at DESC))[1] AS request_id,
                  (array_agg(path ORDER BY at DESC))[1] AS path
           FROM error_events WHERE at >= $1
           GROUP BY fingerprint, source
           ORDER BY max(at) DESC
           LIMIT 40"#,
        start
    )
    .fetch_all(&state.db)
    .await?;
    let error_rows: Vec<minijinja::Value> = errors
        .into_iter()
        .map(|e| {
            minijinja::context! {
                source => e.source,
                count => e.count,
                last => e.last.format("%d.%m %H:%M:%S UTC").to_string(),
                message => e.message,
                request_id => e.request_id,
                path => e.path,
            }
        })
        .collect();

    let firing: Vec<minijinja::Value> = crate::alerts::firing_now()
        .into_iter()
        .map(|(key, minutes)| {
            minijinja::context! {
                label => ctx.t(&format!("monitoring.alert_{key}")),
                minutes => minutes,
            }
        })
        .collect();
    let uptime = crate::metrics::uptime_secs();

    crate::admin::render(
        &ctx,
        "monitoring",
        &ctx.t("monitoring.title"),
        minijinja::context! {
            range => range.key(),
            ranges => ["1h", "24h", "7d"],
            total => minijinja::context! {
                requests => total.requests,
                server_errors => total.server_errors,
                client_errors => total.client_errors,
                error_share => format!("{:.2}", 100.0 * f64::from(total.server_errors) / f64::from(total.requests.max(1))),
                rpm => format!("{:.1}", f64::from(total.requests) / active_minutes),
                p => percentiles(&total),
            },
            peak => minijinja::context! {
                requests => peak_requests,
                rps => format!("{:.1}", f64::from(peak_requests) / 60.0),
                at => chrono::DateTime::from_timestamp(peak_minute * 60, 0)
                    .map(|t| t.format("%d.%m %H:%M UTC").to_string())
                    .unwrap_or_default(),
                in_flight => peak_in_flight,
            },
            chart => minijinja::context! {
                w => CHART_W,
                h => CHART_H,
                bars => bars,
                top_requests => format!("{top_requests:.0}"),
                top_latency => ms(top_latency),
                mid_latency => ms(top_latency / 2.0),
                p50 => line(0.50),
                p95 => line(0.95),
                p99 => line(0.99),
                dots50 => dots(0.50),
                dots95 => dots(0.95),
                dots99 => dots(0.99),
                first => label_of(0),
                middle => label_of(points / 2),
                last => label_of(points - 1),
            },
            routes => route_rows,
            errors => error_rows,
            firing => firing,
            live => minijinja::context! {
                in_flight => crate::metrics::in_flight(),
                uptime => format!("{}h {}m", uptime / 3600, (uptime % 3600) / 60),
                pool_size => state.db.size(),
                pool_idle => state.db.num_idle(),
                memory => resident_mib().map(|m| format!("{m:.0} MiB")),
                bot => crate::bots::any(),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_tops_are_round() {
        assert_eq!(nice(0.0), 1.0);
        assert_eq!(nice(7.0), 10.0);
        assert_eq!(nice(130.0), 200.0);
        assert_eq!(nice(480.0), 500.0);
        assert_eq!(nice(1000.0), 1000.0);
    }

    #[test]
    fn milliseconds_read_well() {
        assert_eq!(ms(3.25), "3.2 ms");
        assert_eq!(ms(250.0), "250 ms");
        assert_eq!(ms(1500.0), "1.50 s");
    }
}

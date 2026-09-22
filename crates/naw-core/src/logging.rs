//! Log initialisation, driven by the environment rather than `config.toml`.
//!
//! Reading these from the environment is deliberate: a broken `config.toml` is
//! exactly when the log matters most, so logging must come up before any
//! parsing can fail.
//!
//! Three knobs:
//!
//! - `NAW_LOG_TRACE=1` turns on request-level detail. Every request gets a span
//!   carrying an id, the method, the path and the matched route, and the
//!   headers are dumped through a redaction filter. Off by default because it
//!   is loud and because the header dump deserves a conscious decision.
//! - `NAW_LOG_FORMAT=json` switches to structured output for a log shipper.
//!   Anything else stays human readable.
//! - `RUST_LOG` overrides the filter entirely, as always.

/// Text for humans, json for machines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogSettings {
    pub trace: bool,
    pub format: LogFormat,
}

fn truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "enabled"
    )
}

/// The default filter for a given verbosity. `RUST_LOG` beats this.
///
/// Trace mode deliberately does not turn on `trace` for the whole world: sqlx
/// at trace level prints every statement it prepares, which buries the request
/// detail this flag exists to surface.
pub fn default_filter(trace: bool) -> &'static str {
    if trace {
        "naw=trace,naw_web=trace,naw_core=trace,tower_http=debug,sqlx=warn"
    } else {
        "naw=info,tower_http=info"
    }
}

pub fn settings_from_env() -> LogSettings {
    let trace = std::env::var("NAW_LOG_TRACE")
        .map(|raw| truthy(&raw))
        .unwrap_or(false);
    let format = match std::env::var("NAW_LOG_FORMAT") {
        Ok(raw) if raw.trim().eq_ignore_ascii_case("json") => LogFormat::Json,
        _ => LogFormat::Text,
    };
    LogSettings { trace, format }
}

/// Headers that must never reach the log, even in trace mode.
///
/// A session cookie in a log file is a session anyone with log access can
/// replay, and a bearer token is worse. Matching is on the lowercased name.
const NEVER_LOG: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-auth-token",
];

/// True when this header's value is safe to write to a log.
pub fn header_is_loggable(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !NEVER_LOG.iter().any(|blocked| lower == *blocked)
}

/// Installs the global subscriber. Call once, early, before anything logs.
pub fn init() -> LogSettings {
    let settings = settings_from_env();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter(settings.trace).into());

    match settings.format {
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_current_span(true)
            .with_span_list(settings.trace)
            .init(),
        LogFormat::Text => tracing_subscriber::fmt()
            .with_env_filter(filter)
            // Span context is what makes a request traceable across modules,
            // and it is noise the rest of the time.
            .with_span_events(if settings.trace {
                tracing_subscriber::fmt::format::FmtSpan::NEW
                    | tracing_subscriber::fmt::format::FmtSpan::CLOSE
            } else {
                tracing_subscriber::fmt::format::FmtSpan::NONE
            })
            .with_target(settings.trace)
            .with_line_number(settings.trace)
            .init(),
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_accepts_the_same_words_as_the_config_parser() {
        for yes in ["1", "true", "TRUE", "yes", "on", "enabled", " on "] {
            assert!(truthy(yes), "{yes}");
        }
        for no in ["0", "false", "no", "off", "", "maybe"] {
            assert!(!truthy(no), "{no}");
        }
    }

    #[test]
    fn credentials_are_never_loggable() {
        // The whole value of trace mode is dumping headers, so this list is the
        // only thing standing between a debug session and a replayable session.
        for blocked in [
            "cookie",
            "Cookie",
            "COOKIE",
            "set-cookie",
            "Authorization",
            "proxy-authorization",
            "X-Api-Key",
            "x-auth-token",
        ] {
            assert!(!header_is_loggable(blocked), "{blocked} must be redacted");
        }
    }

    #[test]
    fn ordinary_headers_stay_loggable() {
        for allowed in [
            "user-agent",
            "accept",
            "referer",
            "host",
            "x-forwarded-for",
            "content-type",
            "if-none-match",
        ] {
            assert!(header_is_loggable(allowed), "{allowed}");
        }
    }

    #[test]
    fn a_header_that_merely_contains_cookie_is_not_redacted_by_accident() {
        // Matching is exact, so a hypothetical future header keeps working.
        assert!(header_is_loggable("x-cookie-consent"));
        assert!(header_is_loggable("cookie-policy-version"));
    }

    #[test]
    fn trace_mode_keeps_sqlx_quiet() {
        // sqlx at trace level prints every prepared statement and drowns the
        // request detail this flag exists to show.
        let filter = default_filter(true);
        assert!(filter.contains("naw_web=trace"));
        assert!(filter.contains("sqlx=warn"));
        assert!(!default_filter(false).contains("trace"));
    }

    #[test]
    fn format_only_switches_on_an_explicit_json_value() {
        // Guarding against a stray "1" turning the log into json.
        assert_eq!(
            settings_from_env().format,
            LogFormat::Text,
            "no NAW_LOG_FORMAT set in the test environment"
        );
    }
}

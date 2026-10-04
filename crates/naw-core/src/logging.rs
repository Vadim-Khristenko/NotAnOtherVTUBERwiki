//! Log setup from the environment, so it works even when `config.toml` does not.
//!
//! - `NAW_LOG_TRACE=1`: per-request spans and a redacted header dump.
//! - `NAW_LOG_FORMAT=json`: structured output.
//! - `RUST_LOG`: overrides the filter.

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

/// The default filter. Trace mode leaves sqlx out, which would log every statement.
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

/// Headers never logged, even in trace mode (lowercased names).
const NEVER_LOG: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-auth-token",
];

pub fn header_is_loggable(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !NEVER_LOG.iter().any(|blocked| lower == *blocked)
}

/// Installs the global subscriber. Call once, before anything logs.
pub fn init() -> LogSettings {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let settings = settings_from_env();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter(settings.trace).into());

    match settings.format {
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_current_span(true)
            .with_span_list(settings.trace)
            .finish()
            .with(ErrorCapture)
            .init(),
        LogFormat::Text => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_span_events(if settings.trace {
                tracing_subscriber::fmt::format::FmtSpan::NEW
                    | tracing_subscriber::fmt::format::FmtSpan::CLOSE
            } else {
                tracing_subscriber::fmt::format::FmtSpan::NONE
            })
            .with_target(settings.trace)
            .with_line_number(settings.trace)
            .finish()
            .with(ErrorCapture)
            .init(),
    }
    settings
}

/// An error the engine logged, kept for the monitoring page and its alerts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedError {
    /// The module that logged it: `naw_web::pages`.
    pub target: String,
    pub message: String,
    /// The other fields, `name=value`, joined by spaces.
    pub fields: String,
    pub request_id: Option<String>,
}

/// Errors waiting for the web layer to store them. Bounded: when nobody
/// drains it, the oldest go first and memory stays flat.
static CAPTURED: std::sync::Mutex<std::collections::VecDeque<CapturedError>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

const CAPTURE_MAX: usize = 500;

/// Longest message or field list kept, in characters.
const CAPTURE_CHARS: usize = 2000;

/// Takes every error captured since the last call.
pub fn take_captured() -> Vec<CapturedError> {
    match CAPTURED.lock() {
        Ok(mut queue) => queue.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

fn capture(error: CapturedError) {
    if let Ok(mut queue) = CAPTURED.lock() {
        if queue.len() >= CAPTURE_MAX {
            queue.pop_front();
        }
        queue.push_back(error);
    }
}

/// Copies every error-level event into [`CAPTURED`].
struct ErrorCapture;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() != tracing::Level::ERROR {
            return;
        }
        let mut visitor = Fields::default();
        event.record(&mut visitor);
        capture(CapturedError {
            target: event.metadata().target().to_string(),
            message: clip(&visitor.message),
            fields: clip(&visitor.rest.join(" ")),
            request_id: visitor.request_id,
        });
    }
}

fn clip(text: &str) -> String {
    text.chars().take(CAPTURE_CHARS).collect()
}

#[derive(Default)]
struct Fields {
    message: String,
    request_id: Option<String>,
    rest: Vec<String>,
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "request_id" => self.request_id = Some(value.to_string()),
            name => self.rest.push(format!("{name}={value}")),
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "request_id" => {
                self.request_id = Some(format!("{value:?}").trim_matches('"').to_string())
            }
            name => self.rest.push(format!("{name}={value:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_events_are_captured_and_others_are_not() {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry().with(ErrorCapture);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("fine");
            tracing::error!(request_id = "abc123def456", error = "boom", "render failed");
        });
        let got: Vec<CapturedError> = take_captured()
            .into_iter()
            .filter(|e| e.message == "render failed")
            .collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].request_id.as_deref(), Some("abc123def456"));
        assert!(got[0].fields.contains("error=\"boom\"") || got[0].fields.contains("error=boom"));
        assert!(take_captured().iter().all(|e| e.message != "fine"));
    }

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
        assert!(header_is_loggable("x-cookie-consent"));
        assert!(header_is_loggable("cookie-policy-version"));
    }

    #[test]
    fn trace_mode_keeps_sqlx_quiet() {
        let filter = default_filter(true);
        assert!(filter.contains("naw_web=trace"));
        assert!(filter.contains("sqlx=warn"));
        assert!(!default_filter(false).contains("trace"));
    }

    #[test]
    fn format_only_switches_on_an_explicit_json_value() {
        assert_eq!(
            settings_from_env().format,
            LogFormat::Text,
            "no NAW_LOG_FORMAT set in the test environment"
        );
    }
}

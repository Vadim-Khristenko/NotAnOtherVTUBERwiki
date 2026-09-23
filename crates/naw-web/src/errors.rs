//! Themed error pages.
//!
//! A middleware turns any error response that is not already HTML into the
//! skin's page for its kind, keeping the status, when the request asked for
//! `text/html`. Other clients (`fetch()`, curl, APIs) keep the original body.
//! A skin may ship `errors/<kind>.html`; `errors/_generic.html` covers the rest.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::resolve::{Ctx, UNKNOWN_WIKI};

/// What went wrong, in the reader's terms; several kinds share a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 404.
    NotFound,
    /// 403: signed in and still not allowed.
    Forbidden,
    /// 401 where a redirect to sign in is not possible.
    Unauthorized,
    /// 409: changed by somebody else first, or taken.
    Conflict,
    /// 400 and 422.
    Invalid,
    /// 405.
    MethodNotAllowed,
    /// 413.
    TooLarge,
    /// 429.
    RateLimited,
    /// A sign-in flow failed; the advice is to try again.
    AuthFailed,
    /// Sign-in is switched off on this install.
    AuthDisabled,
    /// The no-JavaScript sign-in flow wants the next step.
    ManualLogin,
    /// 503 on purpose: maintenance.
    Maintenance,
    /// 502, 503 or 504: a dependency is down.
    Unavailable,
    /// 500.
    Internal,
}

impl Kind {
    /// Every kind, for the admin preview and the tests.
    pub const ALL: [Self; 14] = [
        Self::NotFound,
        Self::Forbidden,
        Self::Unauthorized,
        Self::Conflict,
        Self::Invalid,
        Self::MethodNotAllowed,
        Self::TooLarge,
        Self::RateLimited,
        Self::AuthFailed,
        Self::AuthDisabled,
        Self::ManualLogin,
        Self::Maintenance,
        Self::Unavailable,
        Self::Internal,
    ];

    /// The stable id: template name and message key prefix.
    pub fn id(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::Unauthorized => "unauthorized",
            Self::Conflict => "conflict",
            Self::Invalid => "invalid",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::TooLarge => "too_large",
            Self::RateLimited => "rate_limited",
            Self::AuthFailed => "auth_failed",
            Self::AuthDisabled => "auth_disabled",
            Self::ManualLogin => "manual_login",
            Self::Maintenance => "maintenance",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == raw)
    }

    /// The status served when a handler set none.
    pub fn status(self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Unauthorized | Self::AuthFailed | Self::ManualLogin => StatusCode::UNAUTHORIZED,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Invalid => StatusCode::UNPROCESSABLE_ENTITY,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::AuthDisabled => StatusCode::FORBIDDEN,
            Self::Maintenance | Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The kind of a bare status code.
    pub fn from_status(status: StatusCode) -> Self {
        match status.as_u16() {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 | 410 => Self::NotFound,
            405 => Self::MethodNotAllowed,
            409 => Self::Conflict,
            413 => Self::TooLarge,
            429 => Self::RateLimited,
            400 | 415 | 422 => Self::Invalid,
            502..=504 => Self::Unavailable,
            code if code >= 500 => Self::Internal,
            _ => Self::Invalid,
        }
    }

    /// The sprite icon for the page.
    pub fn icon(self) -> &'static str {
        match self {
            Self::NotFound => "file-text",
            Self::Forbidden | Self::AuthDisabled => "lock",
            Self::Unauthorized | Self::AuthFailed | Self::ManualLogin => "user",
            Self::Conflict => "history",
            Self::Invalid | Self::MethodNotAllowed | Self::TooLarge => "alert-triangle",
            Self::RateLimited => "history",
            Self::Maintenance => "settings",
            Self::Unavailable | Self::Internal => "alert-triangle",
        }
    }

    /// Whether the handler's text may be shown as a detail: yes for client
    /// errors, never for server errors, where it could leak internals.
    pub fn shows_detail(self) -> bool {
        !matches!(self, Self::Internal | Self::Unavailable | Self::Maintenance)
    }
}

/// Set by a handler that knows more than its status. Rendered even on a 2xx
/// (a cancelled sign-in still needs a page). `variant` picks
/// `errors.<kind>.<variant>.*` wording, falling back to the kind's.
#[derive(Clone, Copy, Debug)]
pub struct Marked {
    pub kind: Kind,
    pub variant: Option<&'static str>,
}

/// Set by `observe::layer`, so the page can print the id a report quotes.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Wording for one page when the kind's default is too generic.
#[derive(Default, Clone)]
pub struct Overrides {
    pub title: Option<String>,
    pub message: Option<String>,
    /// Primary action as (href, label).
    pub action: Option<(String, String)>,
}

impl Overrides {
    /// Wording for one variant, falling back to the kind's, plus what depends on
    /// this install. Shared by the error layer and the admin preview.
    pub fn for_page(
        state: &naw_core::state::AppState,
        ctx: &Ctx,
        kind: Kind,
        variant: &str,
    ) -> Self {
        let mut overrides = Self::for_variant(ctx, kind, variant);
        // "No account yet" links to the operator's application page, when set.
        if kind == Kind::AuthFailed
            && variant == "closed"
            && let Some(url) = state.config.auth.apply_url.as_deref()
        {
            overrides.action = Some((url.to_string(), ctx.t("errors.auth_failed.closed.action")));
        }
        overrides
    }

    pub fn for_variant(ctx: &Ctx, kind: Kind, variant: &str) -> Self {
        let lookup = |part: &str| {
            let key = format!("errors.{}.{variant}.{part}", kind.id());
            let text = ctx.t(&key);
            (text != key).then_some(text)
        };
        Self {
            title: lookup("title"),
            message: lookup("body"),
            action: None,
        }
    }
}

/// The kind's own template, or the generic page.
fn template_for(env: &minijinja::Environment<'_>, kind: Kind) -> String {
    let own = format!("errors/{}.html", kind.id());
    if env.get_template(&own).is_ok() {
        own
    } else {
        "errors/_generic.html".to_string()
    }
}

/// Renders an error page with a resolved request context.
pub fn render(
    ctx: &Ctx,
    kind: Kind,
    status: StatusCode,
    detail: Option<&str>,
    request_id: Option<&str>,
    overrides: &Overrides,
) -> Response {
    let env = &ctx.skin.env;
    let name = template_for(env, kind);
    let key = |part: &str| format!("errors.{}.{part}", kind.id());
    let title = overrides
        .title
        .clone()
        .unwrap_or_else(|| ctx.t(&key("title")));
    let message = overrides
        .message
        .clone()
        .unwrap_or_else(|| ctx.t(&key("body")));
    let hint = ctx.t(&key("hint"));
    // A missing hint renders as its key; hints are optional, so drop it.
    let hint = (!hint.starts_with("errors.")).then_some(hint);
    let detail = detail
        .filter(|_| kind.shows_detail())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        // Cut on a character boundary.
        .map(|d| d.chars().take(300).collect::<String>());
    // After a failed sign-in the next step is another attempt.
    let (action_href, action_label) = overrides.action.clone().unwrap_or_else(|| match kind {
        Kind::AuthFailed | Kind::Unauthorized => {
            ("/login".to_string(), ctx.t("errors.back_to_sign_in"))
        }
        _ => ("/".to_string(), ctx.t("errors.back_home")),
    });

    let rendered = env.get_template(&name).and_then(|template| {
        template.render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => title.clone(),
                version => crate::pages::ENGINE_VERSION,
                error_kind => kind.id(),
                error_status => status.as_u16(),
                error_icon => kind.icon(),
                error_title => title,
                error_message => message,
                error_hint => hint,
                error_detail => detail,
                request_id => request_id,
                action_href => action_href,
                action_label => action_label,
                can_retry => matches!(kind, Kind::AuthFailed | Kind::Unavailable | Kind::RateLimited | Kind::Internal),
            }
        })
    });
    match rendered {
        Ok(html) => (
            status,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        // The error page itself failed: plain text, no recursion.
        Err(err) => {
            tracing::error!(error = %err, kind = kind.id(), "error page failed to render");
            (status, status.canonical_reason().unwrap_or("error")).into_response()
        }
    }
}

/// Renders without a resolved wiki.
fn render_bare(
    state: &AppState,
    kind: Kind,
    status: StatusCode,
    request_id: Option<&str>,
) -> Response {
    let skin = state.skin.current();
    let name = template_for(&skin.env, kind);
    let lang = naw_core::i18n::FALLBACK;
    let t = |key: &str| skin.messages.render(lang, key, &[]);
    let rendered = skin.env.get_template(&name).and_then(|template| {
        template.render(minijinja::context! {
            lang => lang,
            dir => "ltr",
            wiki_name => UNKNOWN_WIKI,
            title => t(&format!("errors.{}.title", kind.id())),
            version => crate::pages::ENGINE_VERSION,
            error_kind => kind.id(),
            error_status => status.as_u16(),
            error_icon => kind.icon(),
            error_title => t(&format!("errors.{}.title", kind.id())),
            error_message => t(&format!("errors.{}.body", kind.id())),
            request_id => request_id,
            action_href => "/",
            action_label => t("errors.back_home"),
        })
    });
    match rendered {
        Ok(html) => (
            status,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = %err, kind = kind.id(), "bare error page failed to render");
            (status, status.canonical_reason().unwrap_or("error")).into_response()
        }
    }
}

/// Whether this is a browser asking for a page.
fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

fn is_html(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"))
}

/// A short detail from a plain or JSON error body.
fn detail_from(body: &[u8], content_type: Option<&HeaderValue>) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    let is_json = content_type
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"));
    if is_json {
        // AppError's shape: {"status": 500, "message": "..."}.
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        return value
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string);
    }
    Some(text.to_string())
}

/// The error page middleware.
pub async fn layer(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let headers = req.headers().clone();
    let method = req.method().clone();
    let user = req
        .extensions()
        .get::<Option<CurrentUser>>()
        .cloned()
        .flatten();
    let request_id = req.extensions().get::<RequestId>().map(|r| r.0.clone());
    let response = next.run(req).await;

    let status = response.status();
    let marked = response.extensions().get::<Marked>().copied();
    if marked.is_none() && !status.is_client_error() && !status.is_server_error() {
        return response;
    }
    // HEAD has no body; HTML was rendered on purpose by its handler.
    if method == Method::HEAD || is_html(&response) || !wants_html(&headers) {
        return response;
    }

    let kind = marked
        .map(|m| m.kind)
        .unwrap_or_else(|| Kind::from_status(status));
    let variant = marked.and_then(|m| m.variant);
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    // Error bodies are tiny; the cap guards against a streamed one.
    let body = to_bytes(response.into_body(), 16 * 1024)
        .await
        .unwrap_or_default();
    let detail = detail_from(&body, content_type.as_ref());

    match crate::resolve::context(&state, &headers, user.as_ref()).await {
        Ok(Some(ctx)) => {
            let overrides = variant
                .map(|v| Overrides::for_page(&state, &ctx, kind, v))
                .unwrap_or_default();
            render(
                &ctx,
                kind,
                status,
                detail.as_deref(),
                request_id.as_deref(),
                &overrides,
            )
        }
        _ => render_bare(&state, kind, status, request_id.as_deref()),
    }
}

/// Marks a response with a kind: `refuse(...).marked(Kind::AuthDisabled)`.
pub trait MarkExt {
    fn marked(self, kind: Kind) -> Response;
    fn marked_as(self, kind: Kind, variant: &'static str) -> Response;
}

impl<T: IntoResponse> MarkExt for T {
    fn marked(self, kind: Kind) -> Response {
        let mut response = self.into_response();
        response.extensions_mut().insert(Marked {
            kind,
            variant: None,
        });
        response
    }

    fn marked_as(self, kind: Kind, variant: &'static str) -> Response {
        let mut response = self.into_response();
        response.extensions_mut().insert(Marked {
            kind,
            variant: Some(variant),
        });
        response
    }
}

/// A throwaway body for a kind and its status.
pub fn empty(kind: Kind) -> Response {
    (kind.status(), Body::empty()).marked(kind)
}

/// The 404 every handler returns, rendered by the middleware with the full
/// chrome of the request.
pub fn not_found() -> Response {
    empty(Kind::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_id() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.id()), Some(kind), "{kind:?}");
        }
        assert_eq!(Kind::parse("nope"), None);
    }

    #[test]
    fn a_bare_status_is_recognised_as_the_kind_a_reader_needs() {
        assert_eq!(Kind::from_status(StatusCode::NOT_FOUND), Kind::NotFound);
        assert_eq!(Kind::from_status(StatusCode::FORBIDDEN), Kind::Forbidden);
        assert_eq!(Kind::from_status(StatusCode::CONFLICT), Kind::Conflict);
        assert_eq!(
            Kind::from_status(StatusCode::UNPROCESSABLE_ENTITY),
            Kind::Invalid
        );
        assert_eq!(Kind::from_status(StatusCode::BAD_REQUEST), Kind::Invalid);
        assert_eq!(
            Kind::from_status(StatusCode::PAYLOAD_TOO_LARGE),
            Kind::TooLarge
        );
        assert_eq!(
            Kind::from_status(StatusCode::TOO_MANY_REQUESTS),
            Kind::RateLimited
        );
        assert_eq!(
            Kind::from_status(StatusCode::BAD_GATEWAY),
            Kind::Unavailable
        );
        assert_eq!(
            Kind::from_status(StatusCode::SERVICE_UNAVAILABLE),
            Kind::Unavailable
        );
        assert_eq!(
            Kind::from_status(StatusCode::INTERNAL_SERVER_ERROR),
            Kind::Internal
        );
        assert_eq!(
            Kind::from_status(StatusCode::from_u16(507).unwrap()),
            Kind::Internal
        );
    }

    #[test]
    fn a_server_error_never_shows_its_body() {
        for kind in [Kind::Internal, Kind::Unavailable, Kind::Maintenance] {
            assert!(!kind.shows_detail(), "{kind:?}");
        }
        assert!(Kind::Invalid.shows_detail());
        assert!(Kind::Forbidden.shows_detail());
    }

    #[test]
    fn only_a_browser_navigation_gets_a_page() {
        let mut browser = HeaderMap::new();
        browser.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html,application/xhtml+xml,*/*;q=0.8"),
        );
        assert!(wants_html(&browser));
        let mut fetch = HeaderMap::new();
        fetch.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        assert!(!wants_html(&fetch));
        let mut api = HeaderMap::new();
        api.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        assert!(!wants_html(&api));
        assert!(!wants_html(&HeaderMap::new()));
    }

    #[test]
    fn a_detail_comes_out_of_plain_text_and_out_of_app_error_json() {
        assert_eq!(
            detail_from(b"title: 1 to 200 characters", None).as_deref(),
            Some("title: 1 to 200 characters")
        );
        let json = HeaderValue::from_static("application/json");
        assert_eq!(
            detail_from(br#"{"status":500,"message":"internal error"}"#, Some(&json)).as_deref(),
            Some("internal error")
        );
        assert_eq!(detail_from(b"   ", None), None);
        assert_eq!(detail_from(b"not json", Some(&json)), None);
        assert_eq!(detail_from(&[0xff, 0xfe], None), None);
    }

    #[test]
    fn a_marked_response_keeps_its_kind() {
        let response = (StatusCode::UNAUTHORIZED, "provider said no").marked(Kind::AuthFailed);
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(matches!(
            response.extensions().get::<Marked>(),
            Some(Marked {
                kind: Kind::AuthFailed,
                variant: None
            })
        ));
    }

    #[test]
    fn every_kind_has_its_messages_in_both_shipped_languages() {
        // Every kind needs a title in the shipped packs.
        let dir = format!("{}/../../locales", env!("CARGO_MANIFEST_DIR"));
        let catalog = naw_core::i18n::Catalog::load(&dir).expect("packs load");
        for lang in ["en", "ru"] {
            for kind in Kind::ALL {
                for part in ["title", "body"] {
                    let key = format!("errors.{}.{part}", kind.id());
                    let text = catalog.render(lang, &key, &[]);
                    assert_ne!(text, key, "{lang} is missing {key}");
                }
            }
            assert_ne!(
                catalog.render(lang, "errors.back_home", &[]),
                "errors.back_home"
            );
        }
    }
}

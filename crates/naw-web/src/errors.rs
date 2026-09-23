//! Error pages, recognised automatically and themed by the skin.
//!
//! Handlers across the engine answer failures the quick way:
//! `(StatusCode::FORBIDDEN, "not allowed").into_response()`, a JSON body from
//! `AppError`, or an axum rejection when a form does not parse. Each of those
//! used to reach the browser as a bare line of English on a white page.
//!
//! This module fixes that in one place. A middleware looks at every response on
//! its way out, and when a browser navigation receives an error that is not
//! already HTML, it renders the matching page from the skin instead, keeping the
//! status code. No handler has to change, and no handler can forget.
//!
//! **Skins choose the look per kind.** A page renders `errors/<kind>.html` when
//! the skin or the default skin provides one, and `errors/_generic.html`
//! otherwise, so a skin can restyle "you are not signed in" without touching
//! "the server broke". Messages come from the `errors` area of the language
//! pack, one set per kind.
//!
//! **Machines keep what they asked for.** Only a request whose `Accept` names
//! `text/html` gets a page. `fetch()`, curl and API clients send `*/*` or a JSON
//! type and receive the original body untouched, so the live preview never has a
//! whole document injected into it.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::resolve::{Ctx, UNKNOWN_WIKI};

/// What went wrong, in the words a reader needs, not the status code a server
/// uses. Several kinds share a status: a failed sign-in and a forbidden edit
/// are both refusals, and they need very different pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 404. The address leads nowhere.
    NotFound,
    /// 403. Signed in, and still not allowed.
    Forbidden,
    /// 401. Needs signing in, where a redirect to the login page is not possible.
    Unauthorized,
    /// 409. Somebody else changed it first, or the address is taken.
    Conflict,
    /// 400 and 422. The form or the link was not something we can use.
    Invalid,
    /// 405. Right address, wrong kind of request.
    MethodNotAllowed,
    /// 413. Too big to accept.
    TooLarge,
    /// 429. Too many requests too quickly.
    RateLimited,
    /// A sign-in flow failed: the provider refused, the state did not match, the
    /// link expired. Its own kind because "try again" is the useful advice here
    /// and nowhere else.
    AuthFailed,
    /// Sign-in is switched off on this install.
    AuthDisabled,
    /// The no-JavaScript sign-in flow wants the reader's next step.
    ManualLogin,
    /// 503 on purpose: the operator took the site down to work on it.
    Maintenance,
    /// 502, 503, 504 not on purpose: something the engine depends on is down.
    Unavailable,
    /// 500. Our bug.
    Internal,
}

impl Kind {
    /// Every kind, for the admin panel's preview list and for the tests.
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

    /// The stable identifier: the template name, the message key prefix, and
    /// what a skin author types.
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

    /// The status code this kind is served with when a handler did not set one.
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

    /// Recognises a kind from a bare status code.
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

    /// The icon from the skin's sprite that fronts the page.
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

    /// Whether the handler's own text may be shown as a technical detail.
    ///
    /// For client errors it is ours and it is useful: "title: 1 to 200
    /// characters" tells the reader what to fix. For server errors it is
    /// never shown, because a 500's body is exactly where an internal detail
    /// would leak from, and the reader cannot act on it anyway.
    pub fn shows_detail(self) -> bool {
        !matches!(self, Self::Internal | Self::Unavailable | Self::Maintenance)
    }
}

/// Set on a response by a handler that knows more than its status code says.
/// The middleware renders this kind instead of guessing from the status, and a
/// marked response is rendered even with a 2xx status: a cancelled sign-in is
/// not an error, but it still needs a page.
///
/// `variant` narrows the wording within a kind. `auth_failed` covers a
/// cancelled flow, an expired link and a provider outage, and each reads
/// `errors.auth_failed.<variant>.title` before falling back to the kind's own.
#[derive(Clone, Copy, Debug)]
pub struct Marked {
    pub kind: Kind,
    pub variant: Option<&'static str>,
}

/// Set by `observe::layer`, so the page can print the id a bug report quotes.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Optional wording for one page, when the kind's default message is too
/// generic. The conflict page uses it to say which page changed.
#[derive(Default, Clone)]
pub struct Overrides {
    pub title: Option<String>,
    pub message: Option<String>,
    /// Primary action, as (href, label).
    pub action: Option<(String, String)>,
}

impl Overrides {
    /// The wording for one variant of a kind, where the language pack has it.
    ///
    /// A variant with no messages of its own falls back to the kind's, so a
    /// half-translated pack shows the general "sign-in did not go through"
    /// rather than a raw key.
    /// `for_variant` plus what depends on this install rather than on the
    /// language pack. Shared by the error layer and the admin preview, so the
    /// preview shows the page a reader would get.
    pub fn for_page(
        state: &naw_core::state::AppState,
        ctx: &Ctx,
        kind: Kind,
        variant: &str,
    ) -> Self {
        let mut overrides = Self::for_variant(ctx, kind, variant);
        // "No account yet" is only useful with a way to get one. Where the
        // operator has an application page, that is the way out.
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

/// The template a kind renders with: its own if the skin has one, the generic
/// page otherwise.
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
    // A kind with no hint in the pack renders the key itself; that is visible
    // and fine for a real key, but a hint is optional, so drop it instead.
    let hint = (!hint.starts_with("errors.")).then_some(hint);
    let detail = detail
        .filter(|_| kind.shows_detail())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        // Long bodies are not details, they are documents. Cut on a character
        // boundary so a Cyrillic message never panics the renderer.
        .map(|d| d.chars().take(300).collect::<String>());
    // The way out depends on what failed. After a failed sign-in the useful next
    // step is another attempt, not the front page.
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
        // The error page itself failed. Plain text, the status kept, and the
        // reason in the log: an error page that errors must not recurse.
        Err(err) => {
            tracing::error!(error = %err, kind = kind.id(), "error page failed to render");
            (status, status.canonical_reason().unwrap_or("error")).into_response()
        }
    }
}

/// Renders without a resolved wiki: the host matched nothing, or resolving it
/// is what failed.
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

/// Whether this request is a browser asking for a page.
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

/// Pulls a human-sized detail out of a plain or JSON error body.
fn detail_from(body: &[u8], content_type: Option<&HeaderValue>) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    let is_json = content_type
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"));
    if is_json {
        // AppError's shape: {"status": 500, "message": "internal error"}.
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        return value
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string);
    }
    Some(text.to_string())
}

/// The middleware.
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
    // A HEAD has no body to theme, and a page that is already HTML was rendered
    // on purpose by its handler.
    if method == Method::HEAD || is_html(&response) || !wants_html(&headers) {
        return response;
    }

    let kind = marked
        .map(|m| m.kind)
        .unwrap_or_else(|| Kind::from_status(status));
    let variant = marked.and_then(|m| m.variant);
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    // Error bodies here are one-line strings or tiny JSON. The cap is for the
    // day something unexpected streams a large body through an error status.
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

/// Marks a response with a kind, for a handler that knows better than its
/// status code. `refuse(...).marked(Kind::AuthDisabled)`.
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

/// A throwaway body for handlers that want only a kind and a status.
pub fn empty(kind: Kind) -> Response {
    (kind.status(), Body::empty()).marked(kind)
}

/// The 404 every handler returns. The middleware renders it with the full
/// chrome of the request, so a missing page still has a header, a search box
/// and the reader's language, which the old standalone 404 template did not.
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
        // An unusual 5xx is still ours.
        assert_eq!(
            Kind::from_status(StatusCode::from_u16(507).unwrap()),
            Kind::Internal
        );
    }

    #[test]
    fn a_server_error_never_shows_its_body() {
        // The body of a 500 is exactly where an internal detail would leak from.
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
        // fetch(), curl and API clients keep the original body.
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
        // Not UTF-8 is not a detail.
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
        // A kind without a title renders its message key on the page. Checked
        // here against the real packs, so a new kind cannot ship half-done.
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

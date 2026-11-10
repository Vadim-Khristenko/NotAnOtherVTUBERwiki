//! Two-step sign-in: the code page a fresh session must pass when its owner
//! turned the step on, and `/settings/two-factor`, where they turn it on.
//!
//! There are two ways to get a code, each on or off by itself: an
//! authenticator app (TOTP, the key kept sealed, see [`totp`]), and the wiki's
//! bot, which sends a one-time code to the Telegram or Discord chat the
//! account linked. The bot counts only while a chat is linked and the server
//! runs a bot, so losing either never locks anybody out. Turning the first
//! way on hands out recovery codes for a lost phone or chat.
//!
//! Setting up an app keeps the new key unconfirmed until the first right
//! code, so a half-finished setup locks nobody out. Every change after that
//! asks for a code again: a session left open is not enough to remove the step.

use axum::extract::{Extension, Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rand::Rng;
use serde::Deserialize;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::redirect::{encode_component, safe_next};
use crate::auth::session::{self, CurrentUser};
use crate::auth::{throttle, totp};
use crate::pages::{ENGINE_VERSION, private_page, template_error};
use crate::resolve::Ctx;

const SETTINGS_PAGE: &str = "/settings/two-factor";

/// How long a code from the bot works.
const BOT_CODE_MINUTES: u64 = 5;

/// How soon one session may ask the bot for another code.
const BOT_RESEND_SECS: u64 = 60;

/// Which way a code was taken.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Taken {
    App,
    Bot,
    Recovery,
}

impl Taken {
    fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Bot => "bot",
            Self::Recovery => "recovery_code",
        }
    }
}

/// The key that seals two-step keys, when the server has one.
fn sealing_key(state: &AppState) -> Option<[u8; 32]> {
    state
        .config
        .auth
        .secret_key
        .as_deref()
        .and_then(totp::key_from)
}

/// Where an account stands: its app, its bot, its recovery codes.
struct Methods {
    /// When the app was confirmed; `None` while off or being set up.
    app_since: Option<chrono::DateTime<chrono::Utc>>,
    /// A key being set up, sealed, not confirmed yet.
    setup: Option<Vec<u8>>,
    /// The bot way is switched on.
    bot: bool,
    /// A Telegram or Discord chat is linked.
    chat: bool,
    codes_left: i64,
}

impl Methods {
    /// Whether the bot can send a code right now.
    fn bot_works(&self) -> bool {
        self.bot && self.chat && crate::bots::any()
    }

    fn any_on(&self) -> bool {
        self.app_since.is_some() || self.bot
    }
}

async fn methods(db: &sqlx::PgPool, user_id: Uuid) -> Result<Methods, AppError> {
    let row = sqlx::query!(
        r#"SELECT u.second_factor_bot, c.secret_enc AS "secret_enc?", c.confirmed_at,
                  (EXISTS (SELECT 1 FROM telegram_links t WHERE t.user_id = u.id)
                   OR EXISTS (SELECT 1 FROM discord_links d WHERE d.user_id = u.id)) AS "chat!",
                  (SELECT count(*) FROM recovery_codes r
                   WHERE r.user_id = u.id AND r.used_at IS NULL) AS "codes_left!"
           FROM users u LEFT JOIN totp_credentials c ON c.user_id = u.id
           WHERE u.id = $1"#,
        user_id
    )
    .fetch_one(db)
    .await?;
    Ok(Methods {
        app_since: row.confirmed_at,
        setup: row.secret_enc.filter(|_| row.confirmed_at.is_none()),
        bot: row.second_factor_bot,
        chat: row.chat,
        codes_left: row.codes_left,
    })
}

/// Takes `code` for `user_id`: an app code for a step newer than the last
/// one taken, or an unused recovery code when `recovery` allows it. Each
/// works once, even for two requests at the same moment. A key that does not
/// open (no server secret, or another one) takes no app code.
async fn take_code(
    db: &sqlx::PgPool,
    key: Option<[u8; 32]>,
    user_id: Uuid,
    code: &str,
    recovery: bool,
) -> Result<Option<Taken>, AppError> {
    let row = sqlx::query!(
        "SELECT secret_enc, last_step FROM totp_credentials WHERE user_id = $1",
        user_id
    )
    .fetch_optional(db)
    .await?;
    if let Some(row) = row {
        let secret = key.and_then(|key| totp::open(&key, user_id, &row.secret_enc));
        let now = chrono::Utc::now().timestamp();
        if let Some(step) = secret.and_then(|s| totp::verify(&s, code, now, row.last_step)) {
            let taken = sqlx::query!(
                "UPDATE totp_credentials SET last_step = $2
                 WHERE user_id = $1 AND (last_step IS NULL OR last_step < $2)",
                user_id,
                step
            )
            .execute(db)
            .await?
            .rows_affected()
                > 0;
            return Ok(taken.then_some(Taken::App));
        }
    }
    if !recovery {
        return Ok(None);
    }
    let used = sqlx::query!(
        "UPDATE recovery_codes SET used_at = now()
         WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
        user_id,
        totp::recovery_hash(code)
    )
    .execute(db)
    .await?
    .rows_affected()
        > 0;
    Ok(used.then_some(Taken::Recovery))
}

fn bot_code_key(session_id: Uuid) -> String {
    format!("naw:2fa:bot:{session_id}")
}

fn bot_sent_key(session_id: Uuid) -> String {
    format!("naw:2fa:bot-sent:{session_id}")
}

/// Sends a fresh code for this session to every chat the account linked.
/// `false` when the last one went less than [`BOT_RESEND_SECS`] ago or
/// nothing reached anybody.
async fn send_bot_code(state: &AppState, user_id: Uuid, session_id: Uuid) -> bool {
    let Ok(mut conn) = state.valkey.get().await else {
        return false;
    };
    let fresh: Option<String> = deadpool_redis::redis::cmd("SET")
        .arg(bot_sent_key(session_id))
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(BOT_RESEND_SECS)
        .query_async(&mut conn)
        .await
        .ok()
        .flatten();
    if fresh.is_none() {
        return false;
    }
    let code = format!("{:06}", rand::rng().random_range(0..1_000_000));
    let stored: Result<(), _> = deadpool_redis::redis::cmd("SET")
        .arg(bot_code_key(session_id))
        .arg(hex::encode(crate::auth::token_hash(&code)))
        .arg("EX")
        .arg(BOT_CODE_MINUTES * 60)
        .query_async(&mut conn)
        .await;
    if stored.is_err() {
        return false;
    }
    let (wiki, _) = crate::bots::site(state).await;
    let minutes = BOT_CODE_MINUTES.to_string();
    crate::bots::send_card_to_user(state, user_id, "second_factor_code", |lang| {
        use naw_bots::card::{Card, Tone};
        Card::new(Tone::Info)
            .heading(crate::bots::rich(
                state,
                lang,
                "card_code_title",
                &[("wiki", &wiki)],
            ))
            .text(crate::bots::rich(
                state,
                lang,
                "card_code_text",
                &[("code", &code)],
            ))
            .text(crate::bots::rich(
                state,
                lang,
                "card_code_note",
                &[("minutes", &minutes)],
            ))
            .footer(crate::bots::rich(state, lang, "card_code_footer", &[]))
    })
    .await
}

/// Takes the bot's code for this session; it is gone once taken.
async fn take_bot_code(state: &AppState, session_id: Uuid, code: &str) -> bool {
    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(mut conn) = state.valkey.get().await else {
        return false;
    };
    let key = bot_code_key(session_id);
    let stored: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .ok()
        .flatten();
    if stored.as_deref() != Some(hex::encode(crate::auth::token_hash(&digits)).as_str()) {
        return false;
    }
    // Only the request that deletes it may use it.
    deadpool_redis::redis::cmd("DEL")
        .arg(&key)
        .query_async::<i64>(&mut conn)
        .await
        .is_ok_and(|n| n == 1)
}

/// Any code this account may use here: the app's, the bot's for this
/// session, or (with `recovery`) a recovery code.
async fn take_any(
    state: &AppState,
    session_id: Option<Uuid>,
    user_id: Uuid,
    code: &str,
    recovery: bool,
) -> Result<Option<Taken>, AppError> {
    if let Some(taken) = take_code(&state.db, sealing_key(state), user_id, code, recovery).await? {
        return Ok(Some(taken));
    }
    if let Some(session_id) = session_id
        && methods(&state.db, user_id).await?.bot_works()
        && take_bot_code(state, session_id, code).await
    {
        return Ok(Some(Taken::Bot));
    }
    Ok(None)
}

/// Replaces the account's recovery codes with fresh ones, returned once.
async fn new_recovery_codes(db: &sqlx::PgPool, user_id: Uuid) -> Result<Vec<String>, AppError> {
    let codes: Vec<String> = (0..totp::RECOVERY_CODES)
        .map(|_| totp::recovery_code())
        .collect();
    let hashes: Vec<Vec<u8>> = codes.iter().map(|c| totp::recovery_hash(c)).collect();
    let mut tx = db.begin().await?;
    sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "INSERT INTO recovery_codes (id, user_id, code_hash)
         SELECT gen_random_uuid(), $1, unnest($2::bytea[])",
        user_id,
        &hashes
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(codes)
}

fn render(ctx: &Ctx, status: StatusCode, view: minijinja::Value) -> Result<Response, AppError> {
    let html = ctx
        .skin
        .env
        .get_template("two_factor.html")
        .map_err(template_error)?
        .render(minijinja::context! {
            ..ctx.chrome_context(),
            ..minijinja::context! {
                title => ctx.t("twofactor.title"),
                version => ENGINE_VERSION,
            },
            ..view
        })
        .map_err(template_error)?;
    Ok(private_page(status, html))
}

/// The session in the cookie that still owes its code, with its account.
async fn owing(state: &AppState, headers: &HeaderMap) -> Option<(Uuid, Uuid, String)> {
    let id = session::session_id_from_headers(headers, crate::auth::routes::secure_cookies(state))?;
    sqlx::query!(
        "SELECT s.id, u.id AS user_id, u.username FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.id = $1 AND s.second_factor_pending AND s.expires_at > now()
           AND u.deleted_at IS NULL",
        id
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .map(|row| (row.id, row.user_id, row.username))
}

#[derive(Deserialize, Default)]
pub struct LoginQuery {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    sent: Option<String>,
}

/// GET /login/code. With only the bot on, its code goes out on the way in.
pub async fn login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let Some((session_id, user_id, _)) = owing(&state, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, None).await?);
    let m = methods(&state.db, user_id).await?;
    let sent = query.sent.is_some()
        || (m.bot_works()
            && m.app_since.is_none()
            && send_bot_code(&state, user_id, session_id).await);
    login_form(
        &ctx,
        StatusCode::OK,
        &m,
        &safe_next(query.next.as_deref()),
        sent,
        None,
    )
}

fn login_form(
    ctx: &Ctx,
    status: StatusCode,
    m: &Methods,
    next: &str,
    sent: bool,
    error: Option<&str>,
) -> Result<Response, AppError> {
    render(
        ctx,
        status,
        minijinja::context! {
            mode => "login",
            next => next,
            app => m.app_since.is_some(),
            bot => m.bot_works(),
            sent => sent,
            error => error.map(|key| ctx.t(&format!("twofactor.{key}"))),
        },
    )
}

#[derive(Deserialize)]
pub struct CodeForm {
    #[serde(default)]
    code: String,
    #[serde(default)]
    next: String,
}

/// POST /login/code/send: the bot's code, again.
pub async fn login_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CodeForm>,
) -> Result<Response, AppError> {
    let Some((session_id, user_id, _)) = owing(&state, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    if methods(&state.db, user_id).await?.bot_works() {
        send_bot_code(&state, user_id, session_id).await;
    }
    let next = encode_component(&safe_next(Some(&form.next)));
    Ok(Redirect::to(&format!("/login/code?sent=1&next={next}")).into_response())
}

/// POST /login/code
pub async fn login_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<CodeForm>,
) -> Result<Response, AppError> {
    let Some((session_id, user_id, username)) = owing(&state, &headers).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, None).await?);
    let next = safe_next(Some(&form.next));
    let ip = crate::net::client_ip(&headers, addr, state.config.trust_proxy);
    if throttle::is_blocked(&state.valkey, &username, ip).await {
        let m = methods(&state.db, user_id).await?;
        return login_form(
            &ctx,
            StatusCode::TOO_MANY_REQUESTS,
            &m,
            &next,
            false,
            Some("error_throttled"),
        );
    }
    let Some(taken) = take_any(&state, Some(session_id), user_id, form.code.trim(), true).await?
    else {
        throttle::record_miss(&state.valkey, &username, ip).await;
        let m = methods(&state.db, user_id).await?;
        return login_form(
            &ctx,
            StatusCode::UNAUTHORIZED,
            &m,
            &next,
            false,
            Some("error_code"),
        );
    };
    throttle::clear_account(&state.valkey, &username).await;
    sqlx::query!(
        "UPDATE sessions SET second_factor_pending = false WHERE id = $1",
        session_id
    )
    .execute(&state.db)
    .await?;
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user_id),
            action: "auth.second_factor",
            entity_type: "user",
            entity_id: Some(user_id),
            meta: serde_json::json!({ "with": taken.as_str() }),
        },
    )
    .await;
    Ok(Redirect::to(&next).into_response())
}

fn sign_in_first() -> Response {
    Redirect::to(&format!("/login?next={}", encode_component(SETTINGS_PAGE))).into_response()
}

fn current_session(state: &AppState, headers: &HeaderMap) -> Option<Uuid> {
    session::session_id_from_headers(headers, crate::auth::routes::secure_cookies(state))
}

#[derive(Deserialize, Default)]
pub struct SettingsQuery {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    done: Option<String>,
    #[serde(default)]
    sent: Option<String>,
}

/// GET /settings/two-factor
pub async fn settings_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Query(query): Query<SettingsQuery>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    let m = methods(&state.db, user.id).await?;
    let key = sealing_key(&state);
    let error = crate::pages::message_key(query.error.as_deref(), &["error_"])
        .map(|key| ctx.t(&format!("twofactor.{key}")));
    // An app being set up: the key, as a QR code and as text.
    if let Some(sealed) = &m.setup
        && let Some(secret) = key.and_then(|k| totp::open(&k, user.id, sealed))
    {
        let uri = totp::otpauth_uri(&ctx.wiki.name, &user.username, &secret);
        let grouped = totp::base32(&secret)
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        return render(
            &ctx,
            StatusCode::OK,
            minijinja::context! {
                mode => "setup",
                qr => totp::qr_svg(&uri),
                key => grouped,
                error => error,
            },
        );
    }
    let done = query
        .done
        .as_deref()
        .filter(|d| matches!(*d, "app_on" | "app_off" | "bot_on" | "bot_off"))
        .map(|d| ctx.t(&format!("twofactor.done_{d}")));
    render(
        &ctx,
        StatusCode::OK,
        minijinja::context! {
            mode => "overview",
            app_since => m.app_since.map(|t| ctx.day(t)),
            app_possible => key.is_some(),
            bot => m.bot,
            bot_works => m.bot_works(),
            bot_possible => crate::bots::any(),
            chat => m.chat,
            any_on => m.any_on(),
            codes_left => m.codes_left,
            sent => query.sent.is_some(),
            done => done,
            error => error,
        },
    )
}

fn back_with(key: &str) -> Response {
    Redirect::to(&format!("{SETTINGS_PAGE}?error={key}")).into_response()
}

fn done(what: &str) -> Response {
    Redirect::to(&format!("{SETTINGS_PAGE}?done={what}")).into_response()
}

async fn record(state: &AppState, user_id: Uuid, action: &'static str) {
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user_id),
            action,
            entity_type: "user",
            entity_id: Some(user_id),
            meta: serde_json::json!({}),
        },
    )
    .await;
}

/// The recovery codes, shown this once.
fn codes_page(ctx: &Ctx, codes: Vec<String>, first: bool) -> Result<Response, AppError> {
    render(
        ctx,
        StatusCode::OK,
        minijinja::context! { mode => "codes", codes => codes, first => first },
    )
}

/// After a way was turned on: the recovery codes when the account has none
/// left, which is the first time, or back to the settings.
async fn after_turning_on(
    state: &AppState,
    ctx: &Ctx,
    user_id: Uuid,
    what: &str,
) -> Result<Response, AppError> {
    if methods(&state.db, user_id).await?.codes_left == 0 {
        let codes = new_recovery_codes(&state.db, user_id).await?;
        return codes_page(ctx, codes, true);
    }
    Ok(done(what))
}

/// POST /settings/two-factor/start: a new app key, unconfirmed until a code.
pub async fn start(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let Some(key) = sealing_key(&state) else {
        return Ok(Redirect::to(SETTINGS_PAGE).into_response());
    };
    sqlx::query!(
        "INSERT INTO totp_credentials (user_id, secret_enc) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE
           SET secret_enc = EXCLUDED.secret_enc, last_step = NULL, created_at = now()
           WHERE totp_credentials.confirmed_at IS NULL",
        user.id,
        totp::seal(&key, user.id, &totp::new_secret())
    )
    .execute(&state.db)
    .await?;
    Ok(Redirect::to(SETTINGS_PAGE).into_response())
}

/// POST /settings/two-factor/cancel: drops a key that was never confirmed.
pub async fn cancel(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    sqlx::query!(
        "DELETE FROM totp_credentials WHERE user_id = $1 AND confirmed_at IS NULL",
        user.id
    )
    .execute(&state.db)
    .await?;
    Ok(Redirect::to(SETTINGS_PAGE).into_response())
}

#[derive(Deserialize)]
pub struct ChangeForm {
    #[serde(default)]
    code: String,
}

/// POST /settings/two-factor/enable: the first right code turns the app on.
pub async fn enable(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ChangeForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    if methods(&state.db, user.id).await?.setup.is_none() {
        return Ok(Redirect::to(SETTINGS_PAGE).into_response());
    }
    if take_code(
        &state.db,
        sealing_key(&state),
        user.id,
        form.code.trim(),
        false,
    )
    .await?
    .is_none()
    {
        return Ok(back_with("error_code"));
    }
    sqlx::query!(
        "UPDATE totp_credentials SET confirmed_at = now() WHERE user_id = $1 AND confirmed_at IS NULL",
        user.id
    )
    .execute(&state.db)
    .await?;
    record(&state, user.id, "auth.two_factor_app_on").await;
    after_turning_on(&state, &ctx, user.id, "app_on").await
}

/// POST /settings/two-factor/bot/on: codes from the bot, for a linked chat.
pub async fn bot_on(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    if !crate::bots::any() || !methods(&state.db, user.id).await?.chat {
        return Ok(back_with("error_no_chat"));
    }
    sqlx::query!(
        "UPDATE users SET second_factor_bot = true WHERE id = $1",
        user.id
    )
    .execute(&state.db)
    .await?;
    record(&state, user.id, "auth.two_factor_bot_on").await;
    after_turning_on(&state, &ctx, user.id, "bot_on").await
}

/// POST /settings/two-factor/send: the bot's code for a change here.
pub async fn send(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    if let Some(session_id) = current_session(&state, &headers)
        && methods(&state.db, user.id).await?.bot_works()
    {
        send_bot_code(&state, user.id, session_id).await;
    }
    Ok(Redirect::to(&format!("{SETTINGS_PAGE}?sent=1")).into_response())
}

/// Takes a code for a change on the settings page, or the refusal to send.
async fn confirm(
    state: &AppState,
    headers: &HeaderMap,
    user_id: Uuid,
    code: &str,
    recovery: bool,
) -> Result<Option<Response>, AppError> {
    let session_id = current_session(state, headers);
    Ok(take_any(state, session_id, user_id, code.trim(), recovery)
        .await?
        .is_none()
        .then(|| back_with("error_code")))
}

/// POST /settings/two-factor/codes: fresh recovery codes, for a code.
pub async fn renew_codes(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ChangeForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    let ctx = or_respond!(crate::resolve::required(&state, &headers, Some(&user)).await?);
    if !methods(&state.db, user.id).await?.any_on() {
        return Ok(Redirect::to(SETTINGS_PAGE).into_response());
    }
    if let Some(refused) = confirm(&state, &headers, user.id, &form.code, false).await? {
        return Ok(refused);
    }
    let codes = new_recovery_codes(&state.db, user.id).await?;
    record(&state, user.id, "auth.recovery_codes_renewed").await;
    codes_page(&ctx, codes, false)
}

/// Removes the recovery codes once neither way is on.
async fn drop_codes_if_off(db: &sqlx::PgPool, user_id: Uuid) -> Result<(), AppError> {
    sqlx::query!(
        "DELETE FROM recovery_codes r WHERE r.user_id = $1
           AND NOT EXISTS (SELECT 1 FROM totp_credentials c
                           WHERE c.user_id = $1 AND c.confirmed_at IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM users u WHERE u.id = $1 AND u.second_factor_bot)",
        user_id
    )
    .execute(db)
    .await?;
    Ok(())
}

/// POST /settings/two-factor/disable: the app off, for any code.
pub async fn disable(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ChangeForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    if let Some(refused) = confirm(&state, &headers, user.id, &form.code, true).await? {
        return Ok(refused);
    }
    sqlx::query!("DELETE FROM totp_credentials WHERE user_id = $1", user.id)
        .execute(&state.db)
        .await?;
    drop_codes_if_off(&state.db, user.id).await?;
    record(&state, user.id, "auth.two_factor_app_off").await;
    Ok(done("app_off"))
}

/// POST /settings/two-factor/bot/off: the bot's codes off, for any code.
pub async fn bot_off(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<ChangeForm>,
) -> Result<Response, AppError> {
    let Some(user) = user else {
        return Ok(sign_in_first());
    };
    if let Some(refused) = confirm(&state, &headers, user.id, &form.code, true).await? {
        return Ok(refused);
    }
    sqlx::query!(
        "UPDATE users SET second_factor_bot = false WHERE id = $1",
        user.id
    )
    .execute(&state.db)
    .await?;
    drop_codes_if_off(&state.db, user.id).await?;
    record(&state, user.id, "auth.two_factor_bot_off").await;
    Ok(done("bot_off"))
}

/// Removes both ways and the recovery codes; what an admin uses for someone
/// who lost their phone, their chat and their codes.
pub(crate) async fn turn_off(db: &sqlx::PgPool, user_id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin().await?;
    sqlx::query!("DELETE FROM totp_credentials WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "UPDATE users SET second_factor_bot = false WHERE id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?;
    // A session still waiting for a code would wait forever.
    sqlx::query!(
        "UPDATE sessions SET second_factor_pending = false WHERE user_id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn user(db: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash) VALUES ($1, 'tf', 'tf@example.test', 'x')",
        )
        .bind(id)
        .execute(db)
        .await
        .expect("user");
        id
    }

    fn now_code(secret: &[u8]) -> String {
        let step = chrono::Utc::now().timestamp().div_euclid(totp::STEP_SECS);
        format!("{:06}", totp::code_at(secret, step))
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_code_works_once_and_a_recovery_code_once(db: PgPool) {
        let id = user(&db).await;
        let key = totp::key_from(&"k".repeat(64));
        let secret = totp::new_secret();
        sqlx::query!(
            "INSERT INTO totp_credentials (user_id, secret_enc, confirmed_at) VALUES ($1, $2, now())",
            id,
            totp::seal(&key.expect("key"), id, &secret)
        )
        .execute(&db)
        .await
        .expect("on");
        let code = now_code(&secret);
        let other = totp::key_from(&"j".repeat(64));
        assert!(
            take_code(&db, other, id, &code, true)
                .await
                .expect("other")
                .is_none(),
            "wrong secret"
        );
        assert!(take_code(&db, key, id, &code, true).await.expect("take") == Some(Taken::App));
        assert!(
            take_code(&db, key, id, &code, true)
                .await
                .expect("again")
                .is_none(),
            "replayed"
        );

        let codes = new_recovery_codes(&db, id).await.expect("codes");
        assert_eq!(codes.len(), totp::RECOVERY_CODES);
        assert!(
            take_code(&db, key, id, &codes[0], false)
                .await
                .expect("no")
                .is_none(),
            "not here"
        );
        assert!(
            take_code(&db, key, id, &codes[0], true).await.expect("rec") == Some(Taken::Recovery)
        );
        assert!(
            take_code(&db, key, id, &codes[0], true)
                .await
                .expect("used")
                .is_none()
        );

        turn_off(&db, id).await.expect("off");
        assert!(
            take_code(&db, key, id, &codes[1], true)
                .await
                .expect("gone")
                .is_none()
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recovery_codes_work_without_an_app_and_go_with_the_last_way(db: PgPool) {
        let id = user(&db).await;
        sqlx::query!(
            "UPDATE users SET second_factor_bot = true WHERE id = $1",
            id
        )
        .execute(&db)
        .await
        .expect("bot on");
        let codes = new_recovery_codes(&db, id).await.expect("codes");
        assert!(
            take_code(&db, None, id, &codes[0], true)
                .await
                .expect("rec")
                == Some(Taken::Recovery),
            "no app needed"
        );
        drop_codes_if_off(&db, id).await.expect("kept");
        assert_eq!(
            methods(&db, id).await.expect("m").codes_left,
            9,
            "bot still on"
        );
        sqlx::query!(
            "UPDATE users SET second_factor_bot = false WHERE id = $1",
            id
        )
        .execute(&db)
        .await
        .expect("bot off");
        drop_codes_if_off(&db, id).await.expect("dropped");
        assert_eq!(methods(&db, id).await.expect("m").codes_left, 0);
    }
}

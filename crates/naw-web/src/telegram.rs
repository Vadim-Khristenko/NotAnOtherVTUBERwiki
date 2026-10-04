//! The wiki's Telegram bot.
//!
//! With a bot token in the environment, a signed-in person links their chat
//! from `/settings`: the button opens the bot with a one-time code, and the
//! bot's `/start <code>` ties the chat to the account. From then on the bot
//! can send them a password reset link (asked for on the sign-in page or
//! with `/reset`), tell them about a new sign-in or a changed password, and,
//! if they turn it on, pass on their wiki notifications. `/stop` unlinks.
//!
//! The bot reads its messages by long polling, so the server needs no
//! address Telegram can reach; only one process may read a bot's messages
//! (`telegram_bot_polling`). Sending never blocks a page: alerts go out on
//! their own task, and a failure is logged, never shown.
//!
//! The token is part of every API address, so errors are always stripped of
//! their URL before they are logged.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use naw_core::state::AppState;

const API: &str = "https://api.telegram.org";

/// How long one `getUpdates` call waits for a message.
const POLL_SECS: u64 = 50;

/// How long a link code from the settings page stays good.
pub(crate) const LINK_MINUTES: u64 = 15;

/// Reset links one account may ask the bot for in an hour.
const RESETS_PER_HOUR: i64 = 3;

/// Notifications older than this are not passed on: after downtime, or when
/// forwarding is first turned on, a backlog would only be noise.
const FORWARD_WINDOW_MINUTES: i32 = 10;

pub(crate) struct Bot {
    token: String,
    username: String,
    client: reqwest::Client,
}

static BOT: OnceLock<Bot> = OnceLock::new();

/// The bot, once it has answered `getMe`. `None` without a token, or while
/// Telegram cannot be reached at start.
pub(crate) fn bot() -> Option<&'static Bot> {
    BOT.get()
}

#[derive(Debug)]
pub(crate) enum BotError {
    /// The person blocked the bot or deleted the chat.
    Blocked,
    /// Another process reads this bot's messages.
    Conflict,
    Other(String),
}

impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked => write!(f, "the chat blocked the bot"),
            Self::Conflict => write!(f, "another process reads this bot's updates"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(POLL_SECS + 15))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(crate::auth::http::USER_AGENT)
        .build()
        .unwrap_or_default()
}

/// One Bot API call. The answer's `result`, or why not.
async fn call(
    client: &reqwest::Client,
    token: &str,
    method: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value, BotError> {
    let response = client
        .post(format!("{API}/bot{token}/{method}"))
        .timeout(timeout)
        .json(body)
        .send()
        .await
        // The address carries the token.
        .map_err(|err| BotError::Other(format!("{method}: {}", err.without_url())))?;
    let status = response.status().as_u16();
    let answer: Value = response
        .json()
        .await
        .map_err(|err| BotError::Other(format!("{method}: {}", err.without_url())))?;
    if answer.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(answer.get("result").cloned().unwrap_or(Value::Null));
    }
    let description = answer
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Err(match status {
        403 => BotError::Blocked,
        409 => BotError::Conflict,
        _ => BotError::Other(format!("{method}: {status} {description}")),
    })
}

impl Bot {
    async fn call(&self, method: &str, body: &Value) -> Result<Value, BotError> {
        call(
            &self.client,
            &self.token,
            method,
            body,
            Duration::from_secs(15),
        )
        .await
    }

    /// The bot's @name, without the @.
    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    /// The bot's numeric id: the part of the token before the colon, and
    /// the `client_id` when the same bot is the Telegram login.
    fn id(&self) -> &str {
        self.token.split(':').next().unwrap_or("")
    }

    /// The link that opens the bot and hands it `code`.
    pub(crate) fn start_link(&self, code: &str) -> String {
        format!("https://t.me/{}?start={code}", self.username)
    }

    /// Sends `html` to a chat, with a button under it when `button` is
    /// `(label, https address)`.
    pub(crate) async fn send(
        &self,
        chat_id: i64,
        html: &str,
        button: Option<(&str, &str)>,
    ) -> Result<(), BotError> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "link_preview_options": { "is_disabled": true },
        });
        // Telegram refuses a button to an address that is not public HTTPS.
        if let Some((label, url)) = button.filter(|(_, url)| url.starts_with("https://")) {
            body["reply_markup"] = json!({ "inline_keyboard": [[{ "text": label, "url": url }]] });
        }
        self.call("sendMessage", &body).await.map(|_| ())
    }
}

/// Escapes text for Telegram's HTML.
pub(crate) fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The interface language to write to somebody in: theirs when the wiki
/// has it, else English, else whatever it has.
pub(crate) fn language(state: &AppState, wanted: Option<&str>) -> String {
    let skin = state.skin.current();
    let wanted = wanted
        .unwrap_or("")
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if !wanted.is_empty() && skin.messages.has(&wanted) {
        return wanted;
    }
    if skin.messages.has("en") {
        return "en".to_string();
    }
    skin.messages
        .languages()
        .first()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "en".to_string())
}

/// One message in `lang`, its arguments escaped for HTML.
pub(crate) fn text(state: &AppState, lang: &str, key: &str, args: &[(&str, &str)]) -> String {
    let owned: Vec<(String, String)> = args
        .iter()
        .map(|(k, v)| ((*k).to_string(), esc(v)))
        .collect();
    state
        .skin
        .current()
        .messages
        .render(lang, &format!("telegram.{key}"), &owned)
}

/// The wiki the bot speaks for, and the origin its links start at: the one
/// on the auth base URL's host, else the default wiki, else the first.
pub(crate) async fn site(state: &AppState) -> (String, String) {
    let wikis = crate::resolve::load_wikis(&state.db)
        .await
        .unwrap_or_default();
    let base = state
        .config
        .auth
        .base_url
        .as_deref()
        .map(|b| b.trim_end_matches('/').to_string());
    let host = base
        .as_deref()
        .and_then(|b| b.split("://").nth(1))
        .map(|h| h.split(['/', ':']).next().unwrap_or(h).to_lowercase());
    let wiki = wikis
        .iter()
        .find(|w| w.domain.as_deref().map(str::to_lowercase) == host)
        .or_else(|| wikis.iter().find(|w| w.is_default))
        .or_else(|| wikis.first());
    let name = wiki
        .map(|w| w.name.clone())
        .unwrap_or_else(|| crate::resolve::UNKNOWN_WIKI.to_string());
    let origin = match (wiki.and_then(|w| w.domain.clone()), base) {
        (Some(domain), _) => format!("https://{}", domain.to_lowercase()),
        (None, Some(base)) => base,
        (None, None) => "http://localhost".to_string(),
    };
    (name, origin)
}

/// The chat linked to an account, and the language to write in there: the
/// account's own, else the one Telegram gave when the chat was linked.
async fn chat_of(state: &AppState, user_id: Uuid) -> Option<(i64, String)> {
    let row = sqlx::query!(
        r#"SELECT tl.chat_id, COALESCE(NULLIF(u.locale, ''), tl.language, '') AS "locale!"
           FROM telegram_links tl JOIN users u ON u.id = tl.user_id
           WHERE tl.user_id = $1"#,
        user_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()??;
    Some((row.chat_id, language(state, Some(&row.locale))))
}

/// The language to write to an account in, as [`chat_of`] picks it.
pub(crate) async fn language_of(state: &AppState, user_id: Uuid) -> String {
    match chat_of(state, user_id).await {
        Some((_, lang)) => lang,
        None => language(state, None),
    }
}

/// Records one message sent, or not, for the account's history.
async fn log(state: &AppState, user_id: Uuid, kind: &str, outcome: &Result<(), BotError>) {
    let (status, error) = match outcome {
        Ok(()) => ("sent", None),
        Err(err) => ("failed", Some(err.to_string())),
    };
    let _ = sqlx::query!(
        "INSERT INTO notification_log (id, user_id, kind, channel, status, error)
         VALUES ($1, $2, $3, 'telegram', $4, $5)",
        Uuid::new_v4(),
        user_id,
        kind,
        status,
        error
    )
    .execute(&state.db)
    .await;
    if let Err(BotError::Blocked) = outcome {
        // Nobody to talk to there any more.
        let _ = sqlx::query!("DELETE FROM telegram_links WHERE user_id = $1", user_id)
            .execute(&state.db)
            .await;
    }
}

/// Sends `html` to the account's linked chat, if it has one. `true` when it
/// went.
pub(crate) async fn send_to_user(
    state: &AppState,
    user_id: Uuid,
    kind: &str,
    html: &str,
    button: Option<(&str, &str)>,
) -> bool {
    let Some(bot) = bot() else {
        return false;
    };
    let Some((chat_id, _)) = chat_of(state, user_id).await else {
        return false;
    };
    let outcome = bot.send(chat_id, html, button).await;
    if let Err(err) = &outcome {
        tracing::warn!(error = %err, kind, "a telegram message did not go");
    }
    let sent = outcome.is_ok();
    log(state, user_id, kind, &outcome).await;
    sent
}

/// Whether the account wants security alerts in Telegram (on unless turned off).
async fn wants_alerts(state: &AppState, user_id: Uuid) -> bool {
    sqlx::query_scalar!(
        r#"SELECT COALESCE((settings->'telegram'->>'alerts')::boolean, true) AS "alerts!"
           FROM users WHERE id = $1"#,
        user_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// What happened to the account, for the alert about it.
pub(crate) enum Alert {
    SignIn { device: String },
    PasswordChanged,
    PasswordReset,
}

/// Tells the account's owner in Telegram, on its own task.
pub(crate) fn alert(state: &AppState, user_id: Uuid, what: Alert) {
    if bot().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        if !wants_alerts(&state, user_id).await {
            return;
        }
        let Some((_, lang)) = chat_of(&state, user_id).await else {
            return;
        };
        let username = sqlx::query_scalar!("SELECT username FROM users WHERE id = $1", user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let (wiki, origin) = site(&state).await;
        let when = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string();
        let (kind, key, device) = match &what {
            Alert::SignIn { device } => ("alert_sign_in", "alert_sign_in", device.as_str()),
            Alert::PasswordChanged => ("alert_password", "alert_password_changed", ""),
            Alert::PasswordReset => ("alert_password", "alert_password_reset", ""),
        };
        let device = if device.is_empty() {
            text(&state, &lang, "device_unknown", &[])
        } else {
            device.to_string()
        };
        let body = text(
            &state,
            &lang,
            key,
            &[
                ("wiki", &wiki),
                ("user", &username),
                ("device", &device),
                ("when", &when),
            ],
        );
        let settings = format!("{origin}/settings");
        let label = text(&state, &lang, "open_settings", &[]);
        send_to_user(&state, user_id, kind, &body, Some((&label, &settings))).await;
    });
}

/// Whether the Telegram login with this `client_id` is the wiki's own bot,
/// so a sign-in can grant that bot the right to write.
pub(crate) fn is_login_client(client_id: &str) -> bool {
    let client_id = client_id.trim();
    bot().is_some_and(|b| !client_id.is_empty() && b.id() == client_id)
}

/// After a Telegram sign-in or link: when the person let the bot write
/// (`telegram:bot_access`), their chat becomes the account's, with no code
/// to carry. The chat id comes from the verified id_token. Whether access
/// was granted shows only in whether a message goes through, so a welcome
/// is the test. An account that already has a chat keeps it, and a chat
/// that belongs to another account stays there: only an empty slot fills.
pub(crate) fn adopt_login_chat(
    state: &AppState,
    user_id: Uuid,
    chat_id: i64,
    tg_username: Option<String>,
) {
    let Some(bot) = bot() else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        let taken = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM telegram_links WHERE user_id = $1 OR chat_id = $2) AS "taken!""#,
            user_id,
            chat_id
        )
        .fetch_one(&state.db)
        .await
        .unwrap_or(true);
        if taken {
            return;
        }
        let row = sqlx::query!("SELECT username, locale FROM users WHERE id = $1", user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
        let Some(row) = row else {
            return;
        };
        let lang = language(&state, Some(&row.locale));
        let (wiki, _) = site(&state).await;
        let welcome = text(
            &state,
            &lang,
            "linked_by_login",
            &[("wiki", &wiki), ("user", &row.username)],
        );
        match bot.send(chat_id, &welcome, None).await {
            Ok(()) => {
                match bind(
                    &state.db,
                    user_id,
                    chat_id,
                    tg_username.as_deref(),
                    Some(&lang),
                )
                .await
                {
                    Ok(Bound::Linked(_)) => {}
                    // Taken in between by another account: it keeps the chat.
                    Ok(Bound::Elsewhere(_)) => return,
                    Err(err) => {
                        tracing::error!(error = %err, "could not link the chat a telegram sign-in allowed");
                        return;
                    }
                }
                crate::audit::record_or_log(
                    &state.db,
                    crate::audit::Entry {
                        wiki_id: None,
                        user_id: Some(user_id),
                        action: "auth.telegram_link",
                        entity_type: "user",
                        entity_id: Some(user_id),
                        meta: json!({ "via": "login", "tg_username": tg_username }),
                    },
                )
                .await;
            }
            // The person did not let the bot write: nothing to link.
            Err(BotError::Blocked) => {}
            Err(err) => {
                tracing::warn!(error = %err, "could not greet a chat after a telegram sign-in")
            }
        }
    });
}

/// Starts the bot when a token is set: learns its name, then reads its
/// messages and passes on notifications, each on its own task.
pub fn start(state: AppState) {
    let auth = &state.config.auth;
    let Some(token) = auth.telegram_bot_token.clone() else {
        return;
    };
    if !auth.enabled {
        tracing::info!("a telegram bot token is set but sign-in is off; the bot stays idle");
        return;
    }
    tokio::spawn(async move {
        let client = client();
        let mut wait = 5;
        let username = loop {
            match call(
                &client,
                &token,
                "getMe",
                &json!({}),
                Duration::from_secs(15),
            )
            .await
            {
                Ok(me) => match me.get("username").and_then(Value::as_str) {
                    Some(name) => break name.to_string(),
                    None => tracing::error!("telegram getMe answered without a username"),
                },
                Err(err) => tracing::warn!(error = %err, "the telegram bot is not reachable yet"),
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
            wait = (wait * 2).min(300);
        };
        if BOT
            .set(Bot {
                token,
                username,
                client,
            })
            .is_err()
        {
            return;
        }
        let Some(bot) = bot() else {
            return;
        };
        tracing::info!(bot = %bot.username, "telegram bot ready");
        set_commands(&state, bot).await;
        tokio::spawn(forward_loop(state.clone(), bot));
        if state.config.auth.telegram_bot_polling {
            poll_loop(state, bot).await;
        }
    });
}

/// The command menu, in each language the wiki has.
async fn set_commands(state: &AppState, bot: &Bot) {
    let languages: Vec<String> = state
        .skin
        .current()
        .messages
        .languages()
        .into_iter()
        .map(str::to_string)
        .collect();
    for lang in languages
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(""))
    {
        let shown = if lang.is_empty() {
            language(state, None)
        } else {
            lang.to_string()
        };
        let commands: Vec<Value> = ["link", "unlink", "reset", "help"]
            .iter()
            .map(|c| json!({ "command": c, "description": text(state, &shown, &format!("command_{c}"), &[]) }))
            .collect();
        let mut body = json!({ "commands": commands });
        if !lang.is_empty() {
            body["language_code"] = json!(lang);
        }
        if let Err(err) = bot.call("setMyCommands", &body).await {
            tracing::warn!(error = %err, "could not set the bot's commands");
        }
    }
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    chat: Chat,
    from: Option<From>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct From {
    username: Option<String>,
    language_code: Option<String>,
}

async fn poll_loop(state: AppState, bot: &'static Bot) {
    let mut offset: i64 = 0;
    let mut wait = 1;
    loop {
        let body =
            json!({ "offset": offset, "timeout": POLL_SECS, "allowed_updates": ["message"] });
        match call(
            &bot.client,
            &bot.token,
            "getUpdates",
            &body,
            Duration::from_secs(POLL_SECS + 15),
        )
        .await
        {
            Ok(result) => {
                wait = 1;
                let updates: Vec<Update> = serde_json::from_value(result).unwrap_or_default();
                for update in updates {
                    offset = offset.max(update.update_id + 1);
                    if let Some(message) = update.message {
                        handle(&state, bot, message).await;
                    }
                }
            }
            Err(BotError::Conflict) => {
                tracing::warn!(
                    "another process reads this bot's messages; set NAW_TELEGRAM_BOT_POLLING=false on all but one"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Err(err) => {
                tracing::warn!(error = %err, "reading the bot's messages failed");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                wait = (wait * 2).min(60);
            }
        }
    }
}

/// The command and its argument: `/start@WikiBot code` is `("/start", "code")`.
fn command(text: &str) -> (String, String) {
    let text = text.trim();
    let (head, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let head = head.split('@').next().unwrap_or(head).to_ascii_lowercase();
    (head, rest.trim().to_string())
}

/// The account a chat is linked to.
async fn account_of_chat(state: &AppState, chat_id: i64) -> Option<(Uuid, String)> {
    sqlx::query!(
        "SELECT u.id, u.username FROM telegram_links tl JOIN users u ON u.id = tl.user_id
         WHERE tl.chat_id = $1",
        chat_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()?
    .map(|row| (row.id, row.username))
}

async fn handle(state: &AppState, bot: &Bot, message: Message) {
    // Private chats only: a reset link has no business in a group.
    if message.chat.kind != "private" {
        return;
    }
    let chat_id = message.chat.id;
    let lang = language(
        state,
        message
            .from
            .as_ref()
            .and_then(|f| f.language_code.as_deref()),
    );
    let tg_username = message.from.as_ref().and_then(|f| f.username.clone());
    let (cmd, arg) = command(message.text.as_deref().unwrap_or(""));
    let (wiki, origin) = site(state).await;
    let reply = match cmd.as_str() {
        // `/link <code>` is the same as following the button: for when the
        // t.me link does not open the app, the code can be pasted by hand.
        "/start" | "/link" if !arg.is_empty() => {
            link(state, chat_id, tg_username, &arg, &lang, &wiki).await
        }
        // A link always starts on the site, from the signed-in owner: a code
        // made here and confirmed there could be confirmed by a victim.
        "/link" => text(
            state,
            &lang,
            "link_howto",
            &[
                ("wiki", &wiki),
                ("settings", &format!("{origin}/settings#s-telegram")),
            ],
        ),
        "/reset" => reset(state, chat_id, &lang, &wiki, &origin).await,
        "/stop" | "/unlink" => unlink(state, chat_id, &lang, &wiki).await,
        _ => match account_of_chat(state, chat_id).await {
            Some((_, username)) => text(
                state,
                &lang,
                "help_linked",
                &[("wiki", &wiki), ("user", &username)],
            ),
            None => text(
                state,
                &lang,
                "help",
                &[("wiki", &wiki), ("settings", &format!("{origin}/settings"))],
            ),
        },
    };
    if let Err(err) = bot.send(chat_id, &reply, None).await {
        tracing::warn!(error = %err, "the bot could not answer");
    }
}

fn link_key(code: &str) -> String {
    format!("naw:tg:link:{}", hex::encode(crate::auth::token_hash(code)))
}

/// A one-time code that links the chat that brings it to `user_id`.
pub(crate) async fn new_link_code(state: &AppState, user_id: Uuid) -> Option<String> {
    let code = crate::auth::random_token();
    let mut conn = state.valkey.get().await.ok()?;
    let stored: Result<(), _> = deadpool_redis::redis::cmd("SET")
        .arg(link_key(&code))
        .arg(user_id.to_string())
        .arg("EX")
        .arg(LINK_MINUTES * 60)
        .query_async(&mut conn)
        .await;
    stored.ok().map(|_| code)
}

async fn link(
    state: &AppState,
    chat_id: i64,
    tg_username: Option<String>,
    code: &str,
    lang: &str,
    wiki: &str,
) -> String {
    // Single use: the code is gone once read.
    let user_id: Option<Uuid> = match state.valkey.get().await {
        Ok(mut conn) => deadpool_redis::redis::cmd("GETDEL")
            .arg(link_key(code))
            .query_async::<Option<String>>(&mut conn)
            .await
            .ok()
            .flatten()
            .and_then(|raw| raw.parse().ok()),
        Err(_) => None,
    };
    let Some(user_id) = user_id else {
        return text(state, lang, "link_expired", &[("wiki", wiki)]);
    };
    match bind(
        &state.db,
        user_id,
        chat_id,
        tg_username.as_deref(),
        Some(lang),
    )
    .await
    {
        Ok(Bound::Linked(username)) => {
            crate::audit::record_or_log(
                &state.db,
                crate::audit::Entry {
                    wiki_id: None,
                    user_id: Some(user_id),
                    action: "auth.telegram_link",
                    entity_type: "user",
                    entity_id: Some(user_id),
                    meta: json!({ "tg_username": tg_username }),
                },
            )
            .await;
            text(
                state,
                lang,
                "linked",
                &[("wiki", wiki), ("user", &username)],
            )
        }
        Ok(Bound::Elsewhere(owner)) => text(
            state,
            lang,
            "already_linked",
            &[("wiki", wiki), ("user", &owner)],
        ),
        Err(err) => {
            tracing::error!(error = %err, "could not link a telegram chat");
            text(state, lang, "failed", &[])
        }
    }
}

/// How a link went.
#[derive(Debug, PartialEq, Eq)]
enum Bound {
    /// The chat is the account's now; its username.
    Linked(String),
    /// The chat belongs to another account, whose username this is. It is
    /// not taken from there: a link code passed to somebody else would
    /// otherwise move their chat, and their recovery, to the code's account.
    Elsewhere(String),
}

/// Ties `chat_id` to `user_id`, with the language the person writes to the
/// bot in. An account that had another chat moves to this one.
async fn bind(
    db: &sqlx::PgPool,
    user_id: Uuid,
    chat_id: i64,
    tg_username: Option<&str>,
    language: Option<&str>,
) -> Result<Bound, sqlx::Error> {
    let mut tx = db.begin().await?;
    let owner = sqlx::query!(
        "SELECT tl.user_id, u.username FROM telegram_links tl JOIN users u ON u.id = tl.user_id
         WHERE tl.chat_id = $1 FOR UPDATE OF tl",
        chat_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(owner) = owner.filter(|o| o.user_id != user_id) {
        return Ok(Bound::Elsewhere(owner.username));
    }
    sqlx::query!(
        "INSERT INTO telegram_links (user_id, chat_id, tg_username, language) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE
           SET chat_id = $2, tg_username = $3, language = $4, linked_at = now()",
        user_id,
        chat_id,
        tg_username,
        language
    )
    .execute(&mut *tx)
    .await?;
    let username = sqlx::query_scalar!("SELECT username FROM users WHERE id = $1", user_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Bound::Linked(username))
}

async fn unlink(state: &AppState, chat_id: i64, lang: &str, wiki: &str) -> String {
    let removed = sqlx::query_scalar!(
        "DELETE FROM telegram_links WHERE chat_id = $1 RETURNING user_id",
        chat_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    match removed {
        Some(user_id) => {
            crate::audit::record_or_log(
                &state.db,
                crate::audit::Entry {
                    wiki_id: None,
                    user_id: Some(user_id),
                    action: "auth.telegram_unlink",
                    entity_type: "user",
                    entity_id: Some(user_id),
                    meta: json!({ "from": "bot" }),
                },
            )
            .await;
            text(state, lang, "unlinked", &[("wiki", wiki)])
        }
        None => text(state, lang, "not_linked", &[("wiki", wiki)]),
    }
}

/// `/reset`: a reset link for the linked account, straight in the chat.
async fn reset(state: &AppState, chat_id: i64, lang: &str, wiki: &str, origin: &str) -> String {
    let Some((user_id, username)) = account_of_chat(state, chat_id).await else {
        return text(state, lang, "not_linked", &[("wiki", wiki)]);
    };
    // Without password sign-in the reset page is not served: no dead links.
    if !state.config.auth.password_login {
        return text(state, lang, "reset_off", &[("wiki", wiki)]);
    }
    if !crate::recovery::allow(state, &format!("naw:reset:tg:{user_id}"), RESETS_PER_HOUR).await {
        return text(state, lang, "reset_throttled", &[]);
    }
    if crate::auth::session::install_banned(state, user_id)
        .await
        .unwrap_or(true)
    {
        return text(state, lang, "failed", &[]);
    }
    match crate::recovery::issue(&state.db, user_id, "telegram").await {
        Ok(token) => {
            crate::recovery::requested(state, user_id, "bot").await;
            let url = crate::recovery::reset_url(origin, &token);
            // The link goes in its own message, with a button.
            let body = text(
                state,
                lang,
                "reset_link",
                &[
                    ("wiki", wiki),
                    ("user", &username),
                    ("url", &url),
                    ("minutes", &crate::recovery::RESET_MINUTES.to_string()),
                ],
            );
            let label = text(state, lang, "reset_button", &[]);
            send_to_user(
                state,
                user_id,
                "password_reset",
                &body,
                Some((&label, &url)),
            )
            .await;
            text(state, lang, "reset_sent", &[])
        }
        Err(err) => {
            tracing::error!(error = %err, "could not issue a reset link");
            text(state, lang, "failed", &[])
        }
    }
}

/// Passes new notifications on to the people who asked for it.
async fn forward_loop(state: AppState, bot: &'static Bot) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        if let Err(err) = forward_once(&state, bot).await {
            tracing::warn!(error = %err, "passing notifications on to telegram failed");
        }
    }
}

async fn forward_once(state: &AppState, bot: &Bot) -> Result<(), sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT n.id, n.user_id, n.kind, n.title, n.link, n.note, tl.chat_id,
                  COALESCE(NULLIF(u.locale, ''), tl.language, '') AS "locale!",
                  w.name AS wiki, w.domain,
                  (SELECT a.username FROM users a WHERE a.id = n.actor_id) AS "actor?"
           FROM notifications n
           JOIN telegram_links tl ON tl.user_id = n.user_id
           JOIN users u ON u.id = n.user_id
           JOIN wikis w ON w.id = n.wiki_id
           WHERE n.forwarded_at IS NULL AND n.read_at IS NULL
             AND n.created_at > now() - make_interval(mins => $1)
             AND COALESCE((u.settings->'telegram'->>'notifications')::boolean, false)
           ORDER BY n.created_at
           LIMIT 50"#,
        FORWARD_WINDOW_MINUTES
    )
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let (_, fallback) = site(state).await;
    for row in rows {
        let lang = language(state, Some(&row.locale));
        let who = row.actor.clone().unwrap_or_else(|| {
            state
                .skin
                .current()
                .messages
                .render(&lang, "history.anonymous", &[])
        });
        let line = state.skin.current().messages.render(
            &lang,
            &format!("notify.kind_{}", row.kind),
            &[
                ("who".to_string(), esc(&who)),
                ("title".to_string(), esc(&row.title)),
            ],
        );
        let mut body = format!("<b>{}</b>\n{line}", esc(&row.wiki));
        if let Some(note) = row.note.as_deref().filter(|n| !n.trim().is_empty()) {
            body.push_str(&format!("\n<i>{}</i>", esc(note)));
        }
        let origin = row
            .domain
            .as_deref()
            .map(|d| format!("https://{}", d.to_lowercase()))
            .unwrap_or_else(|| fallback.clone());
        let url = if row.link.starts_with("https://") {
            row.link.clone()
        } else if row.link.starts_with('/') {
            format!("{origin}{}", crate::pages::ascii_location(&row.link))
        } else {
            format!("{origin}/notifications")
        };
        let label = text(state, &lang, "open", &[]);
        let outcome = bot.send(row.chat_id, &body, Some((&label, &url))).await;
        // Marked either way: a message Telegram refused is not retried forever.
        sqlx::query!(
            "UPDATE notifications SET forwarded_at = now() WHERE id = $1",
            row.id
        )
        .execute(&state.db)
        .await?;
        if let Err(err) = &outcome {
            tracing::warn!(error = %err, "a notification did not reach telegram");
        }
        log(
            state,
            row.user_id,
            &format!("notify_{}", row.kind),
            &outcome,
        )
        .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_loses_the_bot_name_and_keeps_its_argument() {
        assert_eq!(
            command("/start abc"),
            ("/start".to_string(), "abc".to_string())
        );
        assert_eq!(
            command("/START@FilianWikiBot  code "),
            ("/start".to_string(), "code".to_string())
        );
        assert_eq!(command("hello"), ("hello".to_string(), String::new()));
        assert_eq!(command(""), (String::new(), String::new()));
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(esc("<b>a & b</b>"), "&lt;b&gt;a &amp; b&lt;/b&gt;");
    }

    #[test]
    fn a_link_code_is_stored_as_a_digest() {
        let key = link_key("code");
        assert!(key.starts_with("naw:tg:link:"));
        assert!(!key.contains("code"));
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use sqlx::PgPool;

    async fn user(db: &PgPool, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash) VALUES ($1, $2, $2 || '@example.test', 'x')",
        )
        .bind(id)
        .bind(name)
        .execute(db)
        .await
        .expect("user");
        id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn a_chat_belongs_to_one_account_at_a_time(db: PgPool) {
        let alice = user(&db, "alice").await;
        let bob = user(&db, "bob").await;
        assert_eq!(
            bind(&db, alice, 42, Some("tg_alice"), Some("ru"))
                .await
                .expect("bind"),
            Bound::Linked("alice".to_string())
        );
        // A code of Bob's opened in Alice's chat does not take it from her.
        assert_eq!(
            bind(&db, bob, 42, None, None).await.expect("bind"),
            Bound::Elsewhere("alice".to_string())
        );
        let owners: Vec<Uuid> =
            sqlx::query_scalar("SELECT user_id FROM telegram_links WHERE chat_id = 42")
                .fetch_all(&db)
                .await
                .expect("owners");
        assert_eq!(owners, [alice], "her recovery stays hers");
        // Linking again moves an account to its new chat.
        assert_eq!(
            bind(&db, alice, 43, None, None).await.expect("rebind"),
            Bound::Linked("alice".to_string())
        );
        let chats: Vec<i64> = sqlx::query_scalar("SELECT chat_id FROM telegram_links")
            .fetch_all(&db)
            .await
            .expect("chats");
        assert_eq!(chats, [43]);
        // The chat she left is free for Bob now.
        assert_eq!(
            bind(&db, bob, 42, None, None).await.expect("bind"),
            Bound::Linked("bob".to_string())
        );
    }
}

//! What the wiki's bots share: their texts, the language they talk in,
//! link codes, and sending a message to a person on every chat they linked.
//!
//! A bot text comes from `bot.<key>` in the locale files and goes through
//! `naw_bots::render` for the platform, so its arguments are escaped there
//! (see that crate). The language is, in order: what the chat chose with
//! /lang, the account's own, the app's (Telegram's `language_code`,
//! Discord's `locale`), English.
//!
//! A link code is made on the settings page by the signed-in owner and
//! works in either bot, once, for [`LINK_MINUTES`]. It is kept in Valkey as
//! a digest.

use naw_bots::Platform;
use naw_bots::card::{Block, Button, Card, Rich, Style, Tone};
use naw_core::state::AppState;
use uuid::Uuid;

/// How long a link code from the settings page stays good.
pub(crate) const LINK_MINUTES: u64 = 15;

/// Notifications older than this are not passed on: after downtime, or when
/// forwarding is first turned on, a backlog would only be noise.
const FORWARD_WINDOW_MINUTES: i32 = 10;

pub(crate) fn platform_key(platform: Platform) -> &'static str {
    match platform {
        Platform::Telegram => "telegram",
        Platform::Discord => "discord",
        Platform::Matrix => "matrix",
    }
}

/// The interface language to write in: `wanted` when the wiki has it, else
/// English, else whatever it has.
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

/// Bot text `key` in `lang` for `platform`, its arguments escaped there.
pub(crate) fn text(
    state: &AppState,
    lang: &str,
    key: &str,
    args: &[(&str, &str)],
    platform: Platform,
) -> String {
    let template = state
        .skin
        .current()
        .messages
        .render(lang, &format!("bot.{key}"), &[]);
    naw_bots::render(&template, args, platform)
}

/// A plain bot text, for a button label or a menu: no markup at all.
pub(crate) fn label(state: &AppState, lang: &str, key: &str) -> String {
    state
        .skin
        .current()
        .messages
        .render(lang, &format!("bot.{key}"), &[])
}

/// The wiki the bots speak for, and the origin their links start at: the
/// one on the auth base URL's host, else the default wiki, else the first.
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

/// Whether any bot runs.
pub(crate) fn any() -> bool {
    crate::telegram::bot().is_some() || crate::discord::bot().is_some()
}

fn link_key(code: &str) -> String {
    format!("naw:link:{}", hex::encode(crate::auth::token_hash(code)))
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

/// The account a code links to; the code is gone once read.
pub(crate) async fn redeem_link_code(state: &AppState, code: &str) -> Option<Uuid> {
    let code = code.trim();
    if code.is_empty() || code.len() > 100 {
        return None;
    }
    let mut conn = state.valkey.get().await.ok()?;
    deadpool_redis::redis::cmd("GETDEL")
        .arg(link_key(code))
        .query_async::<Option<String>>(&mut conn)
        .await
        .ok()
        .flatten()
        .and_then(|raw| raw.parse().ok())
}

/// What a chat chose with /lang.
pub(crate) async fn chat_language(
    db: &sqlx::PgPool,
    platform: Platform,
    chat_id: &str,
) -> Option<String> {
    sqlx::query_scalar!(
        "SELECT language FROM bot_chat_languages WHERE platform = $1 AND chat_id = $2",
        platform_key(platform),
        chat_id
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

pub(crate) async fn set_chat_language(
    db: &sqlx::PgPool,
    platform: Platform,
    chat_id: &str,
    language: &str,
) {
    let _ = sqlx::query!(
        "INSERT INTO bot_chat_languages (platform, chat_id, language) VALUES ($1, $2, $3)
         ON CONFLICT (platform, chat_id) DO UPDATE SET language = $3, chosen_at = now()",
        platform_key(platform),
        chat_id,
        language
    )
    .execute(db)
    .await;
}

/// The language to write to a chat in: its own choice, the account's
/// language, what the app said.
pub(crate) async fn chat_lang(
    state: &AppState,
    platform: Platform,
    chat_id: &str,
    account: Option<Uuid>,
    app: Option<&str>,
) -> String {
    if let Some(chosen) = chat_language(&state.db, platform, chat_id).await {
        return language(state, Some(&chosen));
    }
    if let Some(user_id) = account {
        let locale = sqlx::query_scalar!("SELECT locale FROM users WHERE id = $1", user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        if !locale.is_empty() {
            return language(state, Some(&locale));
        }
    }
    language(state, app)
}

/// Where a person can be reached: their Telegram chat and Discord account.
pub(crate) struct Reach {
    pub telegram: Option<(i64, String)>,
    pub discord: Option<(String, String)>,
}

/// The chats linked to an account, each with the language to write in.
pub(crate) async fn reach(state: &AppState, user_id: Uuid) -> Reach {
    let row = sqlx::query!(
        r#"SELECT u.locale,
                  tl.chat_id AS "tg_chat?", tl.language AS "tg_lang?",
                  dl.discord_id AS "dc_id?", dl.language AS "dc_lang?"
           FROM users u
           LEFT JOIN telegram_links tl ON tl.user_id = u.id
           LEFT JOIN discord_links dl ON dl.user_id = u.id
           WHERE u.id = $1"#,
        user_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return Reach {
            telegram: None,
            discord: None,
        };
    };
    let account = (!row.locale.is_empty()).then(|| row.locale.clone());
    let telegram = match row.tg_chat {
        Some(chat) => {
            let chosen = chat_language(&state.db, Platform::Telegram, &chat.to_string()).await;
            let lang = language(state, chosen.or(account.clone()).or(row.tg_lang).as_deref());
            Some((chat, lang))
        }
        None => None,
    };
    let discord = match row.dc_id {
        Some(id) => {
            let chosen = chat_language(&state.db, Platform::Discord, &id).await;
            let lang = language(state, chosen.or(account.clone()).or(row.dc_lang).as_deref());
            Some((id, lang))
        }
        None => None,
    };
    Reach { telegram, discord }
}

/// Records one message sent, or not, for the account's history.
async fn log(state: &AppState, user_id: Uuid, kind: &str, channel: &str, error: Option<String>) {
    let status = if error.is_some() { "failed" } else { "sent" };
    let _ = sqlx::query!(
        "INSERT INTO notification_log (id, user_id, kind, channel, status, error)
         VALUES ($1, $2, $3, $4, $5, $6)",
        Uuid::new_v4(),
        user_id,
        kind,
        channel,
        status,
        error
    )
    .execute(&state.db)
    .await;
}

/// Sends bot text `key` to every chat the account linked, each in its own
/// language, with an optional button (`label key`, address). `true` when
/// it reached at least one.
pub(crate) async fn send_to_user(
    state: &AppState,
    user_id: Uuid,
    kind: &str,
    key: &str,
    args: &[(&str, &str)],
    button: Option<(&str, &str)>,
) -> bool {
    let reach = reach(state, user_id).await;
    let mut sent = false;
    if let (Some(bot), Some((chat, lang))) = (crate::telegram::bot(), reach.telegram) {
        let body = text(state, &lang, key, args, Platform::Telegram);
        let rows = button
            .map(|(label_key, url)| {
                vec![vec![naw_telegram::Button::Url {
                    label: label(state, &lang, label_key),
                    url: url.to_string(),
                }]]
            })
            .unwrap_or_default();
        match bot.client.send(chat, &body, &rows).await {
            Ok(()) => {
                sent = true;
                log(state, user_id, kind, "telegram", None).await;
            }
            Err(err) => {
                tracing::warn!(error = %err, kind, "a telegram message did not go");
                if matches!(err, naw_telegram::Error::Blocked) {
                    // Nobody to talk to there any more.
                    let _ = sqlx::query!("DELETE FROM telegram_links WHERE user_id = $1", user_id)
                        .execute(&state.db)
                        .await;
                }
                log(state, user_id, kind, "telegram", Some(err.to_string())).await;
            }
        }
    }
    if let (Some(bot), Some((discord_id, lang))) = (crate::discord::bot(), reach.discord) {
        let body = text(state, &lang, key, args, Platform::Discord);
        let buttons: Vec<serde_json::Value> = button
            .filter(|(_, url)| url.starts_with("https://"))
            .map(|(label_key, url)| {
                vec![naw_discord::link_button(
                    &label(state, &lang, label_key),
                    url,
                )]
            })
            .unwrap_or_default();
        match bot.client.dm(&discord_id, &body, &buttons).await {
            Ok(()) => {
                sent = true;
                log(state, user_id, kind, "discord", None).await;
            }
            Err(err) => {
                tracing::warn!(error = %err, kind, "a discord message did not go");
                log(state, user_id, kind, "discord", Some(err.to_string())).await;
            }
        }
    }
    sent
}

/// Sends a card to every chat the account linked, each made in that chat's
/// language by `make`. `true` when it reached at least one.
pub(crate) async fn send_card_to_user(
    state: &AppState,
    user_id: Uuid,
    kind: &str,
    make: impl Fn(&str) -> naw_bots::card::Card,
) -> bool {
    let reach = reach(state, user_id).await;
    let mut sent = false;
    if let (Some(bot), Some((chat, lang))) = (crate::telegram::bot(), reach.telegram) {
        match bot.client.send_card(chat, &make(&lang)).await {
            Ok(()) => {
                sent = true;
                log(state, user_id, kind, "telegram", None).await;
            }
            Err(err) => {
                tracing::warn!(error = %err, kind, "a telegram card did not go");
                if matches!(err, naw_telegram::Error::Blocked) {
                    let _ = sqlx::query!("DELETE FROM telegram_links WHERE user_id = $1", user_id)
                        .execute(&state.db)
                        .await;
                }
                log(state, user_id, kind, "telegram", Some(err.to_string())).await;
            }
        }
    }
    if let (Some(bot), Some((discord_id, lang))) = (crate::discord::bot(), reach.discord) {
        match bot.client.dm_card(&discord_id, &make(&lang)).await {
            Ok(()) => {
                sent = true;
                log(state, user_id, kind, "discord", None).await;
            }
            Err(err) => {
                tracing::warn!(error = %err, kind, "a discord card did not go");
                log(state, user_id, kind, "discord", Some(err.to_string())).await;
            }
        }
    }
    sent
}

/// Bot text `key` in `lang` as a card's rich text, its arguments kept apart.
pub(crate) fn rich(
    state: &AppState,
    lang: &str,
    key: &str,
    args: &[(&str, &str)],
) -> naw_bots::card::Rich {
    let template = state
        .skin
        .current()
        .messages
        .render(lang, &format!("bot.{key}"), &[]);
    args.iter()
        .fold(naw_bots::card::Rich::new(template), |r, (k, v)| {
            r.arg(*k, *v)
        })
}

/// Whether the account wants security alerts from the bots (on unless off).
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

/// Tells the account's owner on every linked chat, on its own task.
pub(crate) fn alert(state: &AppState, user_id: Uuid, what: Alert) {
    if !any() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        if !wants_alerts(&state, user_id).await {
            return;
        }
        let username = sqlx::query_scalar!("SELECT username FROM users WHERE id = $1", user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let (wiki, origin) = site(&state).await;
        let now = chrono::Utc::now();
        let when = now.format("%Y-%m-%d %H:%M UTC").to_string();
        let unix = now.timestamp().to_string();
        let (kind, key, tone, device) = match &what {
            Alert::SignIn { device } => (
                "alert_sign_in",
                "sign_in",
                Tone::Warning,
                Some(device.clone()),
            ),
            Alert::PasswordChanged => ("alert_password", "password_changed", Tone::Danger, None),
            Alert::PasswordReset => ("alert_password", "password_reset", Tone::Danger, None),
        };
        let settings_url = format!("{origin}/settings");
        send_card_to_user(&state, user_id, kind, |lang| {
            let mut fields = Vec::new();
            if let Some(device) = &device {
                let device = if device.is_empty() {
                    label(&state, lang, "device_unknown")
                } else {
                    device.clone()
                };
                fields.push((rich(&state, lang, "card_device", &[]), Rich::plain(device)));
            }
            fields.push((
                rich(&state, lang, "card_when", &[]),
                rich(
                    &state,
                    lang,
                    "card_moment",
                    &[("unix", &unix), ("when", &when)],
                ),
            ));
            Card::new(tone)
                .heading(rich(
                    &state,
                    lang,
                    &format!("card_{key}_title"),
                    &[("wiki", &wiki)],
                ))
                .text(rich(
                    &state,
                    lang,
                    &format!("card_{key}_text"),
                    &[("user", &username)],
                ))
                .block(Block::Fields(fields))
                .footer(rich(&state, lang, &format!("card_{key}_footer"), &[]))
                .row(vec![
                    Button::url(label(&state, lang, "open_settings"), settings_url.clone())
                        .styled(Style::Primary),
                ])
        })
        .await;
    });
}

/// Passes new notifications on to the people who asked for it, on every
/// chat they linked.
pub(crate) async fn forward_loop(state: AppState) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tick.tick().await;
        if let Err(err) = forward_once(&state).await {
            tracing::warn!(error = %err, "passing notifications on to the bots failed");
        }
    }
}

async fn forward_once(state: &AppState) -> Result<(), sqlx::Error> {
    if !any() {
        return Ok(());
    }
    let rows = sqlx::query!(
        r#"SELECT n.id, n.user_id, n.kind, n.title, n.link, n.note, w.name AS wiki, w.domain,
                  (SELECT a.username FROM users a WHERE a.id = n.actor_id) AS "actor?"
           FROM notifications n
           JOIN users u ON u.id = n.user_id
           JOIN wikis w ON w.id = n.wiki_id
           WHERE n.forwarded_at IS NULL AND n.read_at IS NULL
             AND n.created_at > now() - make_interval(mins => $1)
             AND COALESCE((u.settings->'telegram'->>'notifications')::boolean, false)
             AND (EXISTS (SELECT 1 FROM telegram_links tl WHERE tl.user_id = n.user_id)
                  OR EXISTS (SELECT 1 FROM discord_links dl WHERE dl.user_id = n.user_id))
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
        // Marked first: a message a chat refused is not retried forever.
        sqlx::query!(
            "UPDATE notifications SET forwarded_at = now() WHERE id = $1",
            row.id
        )
        .execute(&state.db)
        .await?;
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
        let messages = state.skin.current().messages.clone();
        let tone = match row.kind.as_str() {
            "edit_accepted" | "edit_trusted" => Tone::Success,
            "edit_rejected" => Tone::Danger,
            "edit_waiting" => Tone::Warning,
            _ => Tone::Info,
        };
        let kind = format!("notify_{}", row.kind);
        send_card_to_user(state, row.user_id, &kind, |lang| {
            let who = row
                .actor
                .clone()
                .unwrap_or_else(|| messages.render(lang, "history.anonymous", &[]));
            let line = messages.render(lang, &format!("notify.kind_{}", row.kind), &[]);
            let mut card = Card::new(tone).heading(Rich::plain(row.wiki.clone())).text(
                Rich::new(line)
                    .arg("who", who)
                    .arg("title", row.title.clone()),
            );
            if let Some(note) = row.note.as_deref().filter(|n| !n.trim().is_empty()) {
                card = card.block(Block::Quote(Rich::plain(note)));
            }
            card.row(vec![
                Button::url(label(state, lang, "open"), url.clone()).styled(Style::Primary),
            ])
        })
        .await;
    }
    Ok(())
}

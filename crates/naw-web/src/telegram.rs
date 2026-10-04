//! The wiki's Telegram bot: the conversation, on top of `naw_telegram`.
//!
//! A signed-in person links their chat with a code from `/settings` (the
//! button opens the bot with it, or they send `/link <code>`), or by
//! signing in with Telegram and allowing the bot to write. The bot then
//! answers `/reset` with a password reset link, `/unlink` unlinks, `/lang`
//! changes its language, and the wiki sends alerts and, if wanted,
//! notifications here (see `bots.rs`).
//!
//! The bot reads its messages by long polling, so the server needs no
//! address Telegram can reach; only one process may read a bot's messages
//! (`telegram_bot_polling`). Only private chats are answered.

use std::sync::OnceLock;
use std::time::Duration;

use naw_bots::Platform;
use naw_telegram::{Button, CallbackQuery, Message};
use serde_json::json;
use uuid::Uuid;

use naw_core::state::AppState;

use crate::bots;

/// Reset links one account may ask the bot for in an hour.
const RESETS_PER_HOUR: i64 = 3;

pub(crate) struct TelegramBot {
    pub client: naw_telegram::Client,
    username: String,
}

impl TelegramBot {
    /// The bot's @name, without the @.
    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    /// The link that opens the bot and hands it `code`.
    pub(crate) fn start_link(&self, code: &str) -> String {
        format!("https://t.me/{}?start={code}", self.username)
    }
}

static BOT: OnceLock<TelegramBot> = OnceLock::new();

/// The bot, once it has answered `getMe`. `None` without a token, or while
/// Telegram cannot be reached at start.
pub(crate) fn bot() -> Option<&'static TelegramBot> {
    BOT.get()
}

/// Whether the Telegram login with this `client_id` is the wiki's own bot,
/// so a sign-in can grant that bot the right to write.
pub(crate) fn is_login_client(client_id: &str) -> bool {
    let client_id = client_id.trim();
    bot().is_some_and(|b| !client_id.is_empty() && b.client.bot_id() == client_id)
}

/// How a link went.
#[derive(Debug, PartialEq, Eq)]
enum Bound {
    /// The chat is the account's now; its username.
    Linked(String),
    /// The chat belongs to another account, whose username this is. It is
    /// not taken from there: a code passed to somebody else would otherwise
    /// move their chat, and their recovery, to the code's account.
    Elsewhere(String),
}

/// Ties `chat_id` to `user_id`, with the language the person writes in. An
/// account that had another chat moves to this one.
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

async fn audit_link(
    state: &AppState,
    user_id: Uuid,
    action: &'static str,
    meta: serde_json::Value,
) {
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: None,
            user_id: Some(user_id),
            action,
            entity_type: "user",
            entity_id: Some(user_id),
            meta,
        },
    )
    .await;
}

/// After a Telegram sign-in or link: when the person let the bot write
/// (`telegram:bot_access`), their chat becomes the account's, with no code
/// to carry. The chat id comes from the verified id_token. Whether access
/// was granted shows only in whether a message goes through, so a welcome
/// is the test. Only an empty slot fills: an account with a chat keeps it,
/// a chat of another account stays there.
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
        let Some(username) =
            sqlx::query_scalar!("SELECT username FROM users WHERE id = $1", user_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten()
        else {
            return;
        };
        let lang = bots::chat_lang(
            &state,
            Platform::Telegram,
            &chat_id.to_string(),
            Some(user_id),
            None,
        )
        .await;
        let (wiki, _) = bots::site(&state).await;
        let welcome = bots::text(
            &state,
            &lang,
            "linked_by_login",
            &[("wiki", &wiki), ("user", &username)],
            Platform::Telegram,
        );
        match bot.client.send(chat_id, &welcome, &[]).await {
            Ok(()) => match bind(
                &state.db,
                user_id,
                chat_id,
                tg_username.as_deref(),
                Some(&lang),
            )
            .await
            {
                Ok(Bound::Linked(_)) => {
                    audit_link(
                        &state,
                        user_id,
                        "auth.telegram_link",
                        json!({ "via": "login", "tg_username": tg_username }),
                    )
                    .await;
                }
                Ok(Bound::Elsewhere(_)) => {}
                Err(err) => {
                    tracing::error!(error = %err, "could not link the chat a telegram sign-in allowed")
                }
            },
            // The person did not let the bot write: nothing to link.
            Err(naw_telegram::Error::Blocked) => {}
            Err(err) => {
                tracing::warn!(error = %err, "could not greet a chat after a telegram sign-in")
            }
        }
    });
}

/// Starts the bot when a token is set: learns its name, sets its menu, then
/// reads its messages.
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
        let client = naw_telegram::Client::new(&token);
        let mut wait = 5;
        let username = loop {
            match client.username().await {
                Ok(name) => break name,
                Err(err) => tracing::warn!(error = %err, "the telegram bot is not reachable yet"),
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
            wait = (wait * 2).min(300);
        };
        if BOT.set(TelegramBot { client, username }).is_err() {
            return;
        }
        let Some(bot) = bot() else {
            return;
        };
        tracing::info!(bot = %bot.username, "telegram bot ready");
        set_commands(&state, bot).await;
        if state.config.auth.telegram_bot_polling {
            poll_loop(state, bot).await;
        }
    });
}

/// The command menu, in each language the wiki has, and a default one.
async fn set_commands(state: &AppState, bot: &TelegramBot) {
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
        .map(|l| Some(l.as_str()))
        .chain(std::iter::once(None))
    {
        let shown = bots::language(state, lang);
        let commands: Vec<(String, String)> = ["link", "unlink", "reset", "lang", "help"]
            .iter()
            .map(|c| {
                (
                    c.to_string(),
                    bots::label(state, &shown, &format!("command_{c}")),
                )
            })
            .collect();
        if let Err(err) = bot.client.set_commands(&commands, lang).await {
            tracing::warn!(error = %err, "could not set the bot's commands");
        }
    }
}

async fn poll_loop(state: AppState, bot: &'static TelegramBot) {
    let mut offset: i64 = 0;
    let mut wait = 1;
    loop {
        match bot.client.updates(offset).await {
            Ok(updates) => {
                wait = 1;
                for update in updates {
                    offset = offset.max(update.update_id + 1);
                    if let Some(message) = update.message {
                        handle(&state, bot, message).await;
                    } else if let Some(query) = update.callback_query {
                        callback(&state, bot, query).await;
                    }
                }
            }
            Err(naw_telegram::Error::Conflict) => {
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

/// The language buttons: one per language the wiki has, in its own name.
fn language_buttons(state: &AppState) -> Vec<Vec<Button>> {
    let skin = state.skin.current();
    let buttons: Vec<Button> = skin
        .messages
        .languages()
        .into_iter()
        .map(|code| Button::Callback {
            label: skin
                .messages
                .meta(code)
                .map(|m| m.native_name.clone())
                .unwrap_or_else(|| code.to_uppercase()),
            data: format!("lang:{code}"),
        })
        .collect();
    buttons.chunks(3).map(<[Button]>::to_vec).collect()
}

async fn handle(state: &AppState, bot: &TelegramBot, message: Message) {
    // Private chats only: a reset link has no business in a group.
    if message.chat.kind != "private" {
        return;
    }
    let chat_id = message.chat.id;
    let chat = chat_id.to_string();
    let app_lang = message.from.as_ref().and_then(|f| f.language_code.clone());
    let tg_username = message.from.as_ref().and_then(|f| f.username.clone());
    let account = account_of_chat(state, chat_id).await;
    let lang = bots::chat_lang(
        state,
        Platform::Telegram,
        &chat,
        account.as_ref().map(|a| a.0),
        app_lang.as_deref(),
    )
    .await;
    let (cmd, arg) = naw_telegram::command(message.text.as_deref().unwrap_or(""));
    let (wiki, origin) = bots::site(state).await;
    let settings_url = format!("{origin}/settings");
    let t =
        |key: &str, args: &[(&str, &str)]| bots::text(state, &lang, key, args, Platform::Telegram);
    let mut rows: Vec<Vec<Button>> = Vec::new();
    let reply = match cmd.as_str() {
        // `/link <code>` is the same as following the button, for when the
        // t.me link does not open the app.
        "/start" | "/link" if !arg.is_empty() => {
            link(state, chat_id, tg_username, &arg, &lang, &wiki).await
        }
        // A link always starts on the site, from the signed-in owner: a code
        // made here and confirmed there could be confirmed by a victim.
        "/link" => {
            rows.push(vec![Button::Url {
                label: bots::label(state, &lang, "open_settings"),
                url: format!("{settings_url}#s-telegram"),
            }]);
            t(
                "link_howto",
                &[
                    ("wiki", &wiki),
                    ("settings_url", &format!("{settings_url}#s-telegram")),
                ],
            )
        }
        "/reset" => match account {
            Some((user_id, username)) => {
                match reset(state, user_id, &username, &lang, &wiki, &origin).await {
                    Ok((text, url)) => {
                        rows.push(vec![Button::Url {
                            label: bots::label(state, &lang, "reset_button"),
                            url,
                        }]);
                        text
                    }
                    Err(text) => text,
                }
            }
            None => t("not_linked", &[("wiki", &wiki)]),
        },
        "/stop" | "/unlink" => unlink(state, chat_id, &lang, &wiki).await,
        "/lang" => {
            let wanted = arg.trim().to_ascii_lowercase();
            if !wanted.is_empty() && state.skin.current().messages.has(&wanted) {
                set_language(state, chat_id, &wanted).await;
                bots::text(state, &wanted, "lang_set", &[], Platform::Telegram)
            } else {
                rows = language_buttons(state);
                t(
                    if wanted.is_empty() {
                        "lang_choose"
                    } else {
                        "lang_unknown"
                    },
                    &[],
                )
            }
        }
        _ => match &account {
            Some((_, username)) => t("help_linked", &[("wiki", &wiki), ("user", username)]),
            None => {
                rows.push(vec![Button::Url {
                    label: bots::label(state, &lang, "open_settings"),
                    url: format!("{settings_url}#s-telegram"),
                }]);
                t("help", &[("wiki", &wiki), ("settings_url", &settings_url)])
            }
        },
    };
    if let Err(err) = bot.client.send(chat_id, &reply, &rows).await {
        tracing::warn!(error = %err, "the bot could not answer");
    }
}

/// A pressed language button.
async fn callback(state: &AppState, bot: &TelegramBot, query: CallbackQuery) {
    let Some(code) = query.data.as_deref().and_then(|d| d.strip_prefix("lang:")) else {
        let _ = bot.client.answer_callback(&query.id, "").await;
        return;
    };
    let Some(message) = query.message else {
        return;
    };
    if message.chat.kind != "private" || !state.skin.current().messages.has(code) {
        let _ = bot.client.answer_callback(&query.id, "").await;
        return;
    }
    set_language(state, message.chat.id, code).await;
    let done = bots::text(state, code, "lang_set", &[], Platform::Telegram);
    let _ = bot
        .client
        .answer_callback(&query.id, &bots::label(state, code, "lang_set"))
        .await;
    if let Err(err) = bot.client.send(message.chat.id, &done, &[]).await {
        tracing::warn!(error = %err, "the bot could not confirm a language");
    }
}

/// Remembers a chat's language, on the link too when there is one.
async fn set_language(state: &AppState, chat_id: i64, code: &str) {
    bots::set_chat_language(&state.db, Platform::Telegram, &chat_id.to_string(), code).await;
    let _ = sqlx::query!(
        "UPDATE telegram_links SET language = $2 WHERE chat_id = $1",
        chat_id,
        code
    )
    .execute(&state.db)
    .await;
}

async fn link(
    state: &AppState,
    chat_id: i64,
    tg_username: Option<String>,
    code: &str,
    lang: &str,
    wiki: &str,
) -> String {
    let t =
        |key: &str, args: &[(&str, &str)]| bots::text(state, lang, key, args, Platform::Telegram);
    let Some(user_id) = bots::redeem_link_code(state, code).await else {
        return t("link_expired", &[("wiki", wiki)]);
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
            audit_link(
                state,
                user_id,
                "auth.telegram_link",
                json!({ "tg_username": tg_username }),
            )
            .await;
            t("linked", &[("wiki", wiki), ("user", &username)])
        }
        Ok(Bound::Elsewhere(owner)) => t("already_linked", &[("wiki", wiki), ("user", &owner)]),
        Err(err) => {
            tracing::error!(error = %err, "could not link a telegram chat");
            t("failed", &[])
        }
    }
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
            audit_link(
                state,
                user_id,
                "auth.telegram_unlink",
                json!({ "from": "bot" }),
            )
            .await;
            bots::text(
                state,
                lang,
                "unlinked",
                &[("wiki", wiki)],
                Platform::Telegram,
            )
        }
        None => bots::text(
            state,
            lang,
            "not_linked",
            &[("wiki", wiki)],
            Platform::Telegram,
        ),
    }
}

/// `/reset`: a reset link for the linked account, in this chat. The text
/// and the address for its button, or the refusal to show instead.
pub(crate) async fn reset(
    state: &AppState,
    user_id: Uuid,
    username: &str,
    lang: &str,
    wiki: &str,
    origin: &str,
) -> Result<(String, String), String> {
    reset_for(
        state,
        user_id,
        username,
        lang,
        wiki,
        origin,
        Platform::Telegram,
        "telegram",
    )
    .await
}

/// The reset flow both bots share: checks, throttle, a fresh link.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reset_for(
    state: &AppState,
    user_id: Uuid,
    username: &str,
    lang: &str,
    wiki: &str,
    origin: &str,
    platform: Platform,
    channel: &str,
) -> Result<(String, String), String> {
    let t = |key: &str, args: &[(&str, &str)]| bots::text(state, lang, key, args, platform);
    // Without password sign-in the reset page is not served: no dead links.
    if !state.config.auth.password_login {
        return Err(t("reset_off", &[("wiki", wiki)]));
    }
    if !crate::recovery::allow(state, &format!("naw:reset:bot:{user_id}"), RESETS_PER_HOUR).await {
        return Err(t("reset_throttled", &[]));
    }
    if crate::auth::session::install_banned(state, user_id)
        .await
        .unwrap_or(true)
    {
        return Err(t("failed", &[]));
    }
    match crate::recovery::issue(&state.db, user_id, channel).await {
        Ok(token) => {
            crate::recovery::requested(state, user_id, channel).await;
            let url = crate::recovery::reset_url(origin, &token);
            let minutes = crate::recovery::RESET_MINUTES.to_string();
            Ok((
                t(
                    "reset_link",
                    &[
                        ("wiki", wiki),
                        ("user", username),
                        ("url", &url),
                        ("minutes", &minutes),
                    ],
                ),
                url,
            ))
        }
        Err(err) => {
            tracing::error!(error = %err, "could not issue a reset link");
            Err(t("failed", &[]))
        }
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
        // Linking again moves an account to its new chat, and frees the old.
        assert_eq!(
            bind(&db, alice, 43, None, None).await.expect("rebind"),
            Bound::Linked("alice".to_string())
        );
        assert_eq!(
            bind(&db, bob, 42, None, None).await.expect("bind"),
            Bound::Linked("bob".to_string())
        );
    }
}

//! The wiki's Discord bot: slash commands over the interactions endpoint.
//!
//! Discord posts each command to `/discord/interactions`, signed with the
//! application's key; an unsigned or wrongly signed request is refused
//! before anything reads it. Answers go back in the same response and only
//! the person who asked sees them, which is also how `/reset` hands over
//! its link: no direct message needed. Alerts and notifications go by
//! direct message, which works when the person shares a server with the
//! bot or added the app to their account.
//!
//! Commands: `/link <code>` (the code from the settings page), `/unlink`,
//! `/reset`, `/lang`, `/help`, registered globally with their Russian
//! names and descriptions beside the English ones. Signing in with Discord
//! links the account too, since the login already proved whose it is.

use std::sync::OnceLock;

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use naw_bots::Platform;
use serde_json::{Value, json};
use uuid::Uuid;

use naw_core::state::AppState;

use crate::bots;

pub(crate) struct DiscordBot {
    pub client: naw_discord::Client,
    public_key: String,
}

static BOT: OnceLock<DiscordBot> = OnceLock::new();

/// The bot, when a token, a public key and an application id are set.
pub(crate) fn bot() -> Option<&'static DiscordBot> {
    BOT.get()
}

impl DiscordBot {
    pub(crate) fn install_url(&self) -> String {
        self.client.install_url()
    }
}

/// Sets the bot up and registers its commands, when it is configured.
pub fn start(state: AppState) {
    let auth = &state.config.auth;
    let (Some(token), Some(public_key), Some(app_id)) = (
        auth.discord_bot_token.clone(),
        auth.discord_public_key.clone(),
        auth.discord_app_id.clone(),
    ) else {
        return;
    };
    if !auth.enabled {
        return;
    }
    let client = naw_discord::Client::new(&token, &app_id);
    if BOT.set(DiscordBot { client, public_key }).is_err() {
        return;
    }
    tokio::spawn(async move {
        let Some(bot) = bot() else {
            return;
        };
        match bot.client.register_commands(&commands(&state)).await {
            Ok(()) => tracing::info!("discord bot ready, commands registered"),
            Err(err) => tracing::warn!(error = %err, "could not register the discord commands"),
        }
    });
}

/// The slash commands, with a name and description in every language the
/// wiki has (Discord shows each person theirs).
fn commands(state: &AppState) -> Value {
    let skin = state.skin.current();
    let langs: Vec<String> = skin
        .messages
        .languages()
        .into_iter()
        .filter(|l| *l != "en")
        .map(str::to_string)
        .collect();
    let en = bots::language(state, Some("en"));
    let described = |key: &str| -> (String, Value) {
        let mut local = serde_json::Map::new();
        for lang in &langs {
            local.insert(lang.clone(), json!(bots::label(state, lang, key)));
        }
        (bots::label(state, &en, key), Value::Object(local))
    };
    let command = |name: &str, options: Value| -> Value {
        let (description, localized) = described(&format!("command_{name}"));
        json!({
            "name": name,
            "type": 1,
            "description": description,
            "description_localizations": localized,
            "options": options,
            // In servers, in DMs with the bot, and anywhere for people who
            // added the app to their account.
            "integration_types": [0, 1],
            "contexts": [0, 1, 2],
        })
    };
    let (code_desc, code_local) = described("command_link_code");
    let (lang_desc, lang_local) = described("command_lang_option");
    let choices: Vec<Value> = skin
        .messages
        .languages()
        .into_iter()
        .map(|code| {
            json!({
                "name": skin.messages.meta(code).map(|m| m.native_name.clone()).unwrap_or_else(|| code.to_uppercase()),
                "value": code,
            })
        })
        .collect();
    json!([
        command(
            "link",
            json!([{ "type": 3, "name": "code", "description": code_desc, "description_localizations": code_local, "required": true }])
        ),
        command("unlink", json!([])),
        command("reset", json!([])),
        command(
            "lang",
            json!([{ "type": 3, "name": "language", "description": lang_desc, "description_localizations": lang_local, "required": true, "choices": choices }])
        ),
        command("help", json!([])),
    ])
}

/// How a link went.
#[derive(Debug, PartialEq, Eq)]
enum Bound {
    Linked(String),
    /// The Discord account is linked to another wiki account; it stays.
    Elsewhere(String),
}

async fn bind(
    db: &sqlx::PgPool,
    user_id: Uuid,
    discord_id: &str,
    username: Option<&str>,
    language: Option<&str>,
) -> Result<Bound, sqlx::Error> {
    let mut tx = db.begin().await?;
    let owner = sqlx::query!(
        "SELECT dl.user_id, u.username FROM discord_links dl JOIN users u ON u.id = dl.user_id
         WHERE dl.discord_id = $1 FOR UPDATE OF dl",
        discord_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(owner) = owner.filter(|o| o.user_id != user_id) {
        return Ok(Bound::Elsewhere(owner.username));
    }
    sqlx::query!(
        "INSERT INTO discord_links (user_id, discord_id, username, language) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE
           SET discord_id = $2, username = $3, language = $4, linked_at = now()",
        user_id,
        discord_id,
        username,
        language
    )
    .execute(&mut *tx)
    .await?;
    let name = sqlx::query_scalar!("SELECT username FROM users WHERE id = $1", user_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Bound::Linked(name))
}

/// After a Discord sign-in or link: the account's Discord, proven by the
/// login, becomes its bot contact too, when that slot is empty.
pub(crate) fn adopt_login(
    state: &AppState,
    user_id: Uuid,
    discord_id: String,
    username: Option<String>,
) {
    if bot().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let taken = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM discord_links WHERE user_id = $1 OR discord_id = $2) AS "taken!""#,
            user_id,
            discord_id
        )
        .fetch_one(&state.db)
        .await
        .unwrap_or(true);
        if taken {
            return;
        }
        if let Ok(Bound::Linked(_)) =
            bind(&state.db, user_id, &discord_id, username.as_deref(), None).await
        {
            audit(
                &state,
                user_id,
                "auth.discord_link",
                json!({ "via": "login" }),
            )
            .await;
        }
    });
}

async fn audit(state: &AppState, user_id: Uuid, action: &'static str, meta: Value) {
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

async fn account_of(state: &AppState, discord_id: &str) -> Option<(Uuid, String)> {
    sqlx::query!(
        "SELECT u.id, u.username FROM discord_links dl JOIN users u ON u.id = dl.user_id
         WHERE dl.discord_id = $1",
        discord_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()?
    .map(|row| (row.id, row.username))
}

/// POST /discord/interactions
pub async fn interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(bot) = bot() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    };
    if !naw_discord::verify(
        &bot.public_key,
        header("x-signature-ed25519"),
        header("x-signature-timestamp"),
        &body,
    ) {
        return (StatusCode::UNAUTHORIZED, "invalid request signature").into_response();
    }
    let Ok(interaction) = serde_json::from_slice::<naw_discord::Interaction>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match interaction.kind {
        naw_discord::PING => Json(naw_discord::pong()).into_response(),
        naw_discord::COMMAND => Json(command(&state, &interaction).await).into_response(),
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn command(state: &AppState, interaction: &naw_discord::Interaction) -> Value {
    let Some(author) = interaction.author() else {
        return naw_discord::ephemeral("?", &[]);
    };
    let discord_id = author.id.clone();
    let account = account_of(state, &discord_id).await;
    let app_lang = naw_discord::language(interaction.locale.as_deref());
    let lang = bots::chat_lang(
        state,
        Platform::Discord,
        &discord_id,
        account.as_ref().map(|a| a.0),
        app_lang.as_deref(),
    )
    .await;
    let (wiki, origin) = bots::site(state).await;
    let settings_url = format!("{origin}/settings#s-discord");
    let t =
        |key: &str, args: &[(&str, &str)]| bots::text(state, &lang, key, args, Platform::Discord);
    let settings_button = || {
        vec![naw_discord::link_button(
            &bots::label(state, &lang, "open_settings"),
            &settings_url,
        )]
    };
    let name = interaction
        .data
        .as_ref()
        .map(|d| d.name.as_str())
        .unwrap_or("");
    match name {
        "link" => {
            let code = interaction.option("code").unwrap_or("");
            let Some(user_id) = bots::redeem_link_code(state, code).await else {
                return naw_discord::ephemeral(
                    &t("link_expired", &[("wiki", &wiki)]),
                    &settings_button(),
                );
            };
            match bind(
                &state.db,
                user_id,
                &discord_id,
                author.username.as_deref(),
                Some(&lang),
            )
            .await
            {
                Ok(Bound::Linked(username)) => {
                    audit(
                        state,
                        user_id,
                        "auth.discord_link",
                        json!({ "discord_username": author.username }),
                    )
                    .await;
                    naw_discord::ephemeral(
                        &t("linked", &[("wiki", &wiki), ("user", &username)]),
                        &[],
                    )
                }
                Ok(Bound::Elsewhere(owner)) => naw_discord::ephemeral(
                    &t("already_linked", &[("wiki", &wiki), ("user", &owner)]),
                    &[],
                ),
                Err(err) => {
                    tracing::error!(error = %err, "could not link a discord account");
                    naw_discord::ephemeral(&t("failed", &[]), &[])
                }
            }
        }
        "unlink" => {
            let removed = sqlx::query_scalar!(
                "DELETE FROM discord_links WHERE discord_id = $1 RETURNING user_id",
                discord_id
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
            match removed {
                Some(user_id) => {
                    audit(
                        state,
                        user_id,
                        "auth.discord_unlink",
                        json!({ "from": "bot" }),
                    )
                    .await;
                    naw_discord::ephemeral(&t("unlinked", &[("wiki", &wiki)]), &[])
                }
                None => {
                    naw_discord::ephemeral(&t("not_linked", &[("wiki", &wiki)]), &settings_button())
                }
            }
        }
        "reset" => match account {
            Some((user_id, username)) => {
                match crate::telegram::reset_for(
                    state,
                    user_id,
                    &username,
                    &lang,
                    &wiki,
                    &origin,
                    Platform::Discord,
                    "discord",
                )
                .await
                {
                    Ok((text, url)) => naw_discord::ephemeral(
                        &text,
                        &[naw_discord::link_button(
                            &bots::label(state, &lang, "reset_button"),
                            &url,
                        )],
                    ),
                    Err(text) => naw_discord::ephemeral(&text, &[]),
                }
            }
            None => {
                naw_discord::ephemeral(&t("not_linked", &[("wiki", &wiki)]), &settings_button())
            }
        },
        "lang" => {
            let wanted = interaction
                .option("language")
                .unwrap_or("")
                .to_ascii_lowercase();
            if state.skin.current().messages.has(&wanted) {
                bots::set_chat_language(&state.db, Platform::Discord, &discord_id, &wanted).await;
                let _ = sqlx::query!(
                    "UPDATE discord_links SET language = $2 WHERE discord_id = $1",
                    discord_id,
                    wanted
                )
                .execute(&state.db)
                .await;
                naw_discord::ephemeral(
                    &bots::text(state, &wanted, "lang_set", &[], Platform::Discord),
                    &[],
                )
            } else {
                naw_discord::ephemeral(&t("lang_unknown", &[]), &[])
            }
        }
        _ => match account {
            Some((_, username)) => naw_discord::ephemeral(
                &t("help_linked", &[("wiki", &wiki), ("user", &username)]),
                &[],
            ),
            None => naw_discord::ephemeral(
                &t("help", &[("wiki", &wiki), ("settings_url", &settings_url)]),
                &settings_button(),
            ),
        },
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
    async fn a_discord_account_belongs_to_one_wiki_account(db: PgPool) {
        let alice = user(&db, "alice").await;
        let bob = user(&db, "bob").await;
        assert_eq!(
            bind(&db, alice, "111", Some("al"), None)
                .await
                .expect("bind"),
            Bound::Linked("alice".into())
        );
        assert_eq!(
            bind(&db, bob, "111", None, None).await.expect("bind"),
            Bound::Elsewhere("alice".into())
        );
        assert_eq!(
            bind(&db, alice, "222", None, None).await.expect("move"),
            Bound::Linked("alice".into())
        );
        assert_eq!(
            bind(&db, bob, "111", None, None).await.expect("free"),
            Bound::Linked("bob".into())
        );
    }
}

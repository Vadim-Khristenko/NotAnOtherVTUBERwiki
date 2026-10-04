//! A small Telegram Bot API client for the wiki's bot.
//!
//! Messages go out as rich messages (`sendRichMessage` with an `html`
//! body), which know headings, quotes, spoilers and custom emoji. Where the
//! method is missing (an older Bot API server) or refuses the markup, the
//! same text goes again through `sendMessage` in the basic HTML mode, so a
//! message is never lost to formatting. Once the rich method is known to
//! be missing, the client stops trying it.
//!
//! The token is part of every address, so every error is stripped of its
//! URL before anyone can log it.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

const API: &str = "https://api.telegram.org";

/// How long one `getUpdates` call waits for a message.
pub const POLL_SECS: u64 = 50;

#[derive(Debug)]
pub enum Error {
    /// The person blocked the bot or deleted the chat.
    Blocked,
    /// Another process reads this bot's updates.
    Conflict,
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked => write!(f, "the chat blocked the bot"),
            Self::Conflict => write!(f, "another process reads this bot's updates"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

/// A button under a message: an address to open, or data the bot gets back.
#[derive(Clone, Debug)]
pub enum Button {
    Url { label: String, url: String },
    Callback { label: String, data: String },
}

impl Button {
    fn json(&self) -> Option<Value> {
        match self {
            // Telegram refuses a button to an address that is not public HTTPS.
            Self::Url { label, url } if url.starts_with("https://") => {
                Some(json!({ "text": label, "url": url }))
            }
            Self::Url { .. } => None,
            Self::Callback { label, data } => Some(json!({ "text": label, "callback_data": data })),
        }
    }
}

/// Rich support: not tried yet, works, or missing.
const UNKNOWN: u8 = 0;
const RICH: u8 = 1;
const BASIC: u8 = 2;

pub struct Client {
    token: String,
    http: reqwest::Client,
    rich: AtomicU8,
}

#[derive(Deserialize, Debug)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Deserialize, Debug)]
pub struct Message {
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize, Debug)]
pub struct User {
    pub id: i64,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub data: Option<String>,
}

impl Client {
    pub fn new(token: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(POLL_SECS + 15))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("NotAnotherWiki/0.1 (+https://snackers.wiki)")
            .build()
            .unwrap_or_default();
        Self {
            token: token.trim().to_string(),
            http,
            rich: AtomicU8::new(UNKNOWN),
        }
    }

    /// The bot's numeric id: the token before the colon, and the `client_id`
    /// when the same bot is the Telegram login.
    pub fn bot_id(&self) -> &str {
        self.token.split(':').next().unwrap_or("")
    }

    /// One Bot API call; the answer's `result`, or why not.
    pub async fn call(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        let response = self
            .http
            .post(format!("{API}/bot{}/{method}", self.token))
            .timeout(timeout)
            .json(body)
            .send()
            .await
            // The address carries the token.
            .map_err(|err| Error::Other(format!("{method}: {}", err.without_url())))?;
        let status = response.status().as_u16();
        let answer: Value = response
            .json()
            .await
            .map_err(|err| Error::Other(format!("{method}: {}", err.without_url())))?;
        if answer.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(answer.get("result").cloned().unwrap_or(Value::Null));
        }
        let description = answer
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Err(match status {
            403 => Error::Blocked,
            409 => Error::Conflict,
            _ => Error::Other(format!("{method}: {status} {description}")),
        })
    }

    async fn quick(&self, method: &str, body: &Value) -> Result<Value, Error> {
        self.call(method, body, Duration::from_secs(15)).await
    }

    /// The bot's @name.
    pub async fn username(&self) -> Result<String, Error> {
        let me = self.quick("getMe", &json!({})).await?;
        me.get("username")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Other("getMe answered without a username".to_string()))
    }

    /// Sends rich HTML to a chat, with buttons in rows under it.
    pub async fn send(&self, chat_id: i64, html: &str, rows: &[Vec<Button>]) -> Result<(), Error> {
        let keyboard: Vec<Vec<Value>> = rows
            .iter()
            .map(|row| row.iter().filter_map(Button::json).collect::<Vec<_>>())
            .filter(|row| !row.is_empty())
            .collect();
        let markup = (!keyboard.is_empty()).then(|| json!({ "inline_keyboard": keyboard }));
        if self.rich.load(Ordering::Relaxed) != BASIC {
            let mut body = json!({
                "chat_id": chat_id,
                "rich_message": { "html": html, "skip_entity_detection": false },
            });
            if let Some(markup) = &markup {
                body["reply_markup"] = markup.clone();
            }
            match self.quick("sendRichMessage", &body).await {
                Ok(_) => {
                    self.rich.store(RICH, Ordering::Relaxed);
                    return Ok(());
                }
                Err(Error::Blocked) => return Err(Error::Blocked),
                Err(Error::Other(detail))
                    if detail.contains(" 404 ")
                        || detail.to_lowercase().contains("method not found") =>
                {
                    tracing::info!(
                        "this Bot API has no rich messages; sending basic HTML from now on"
                    );
                    self.rich.store(BASIC, Ordering::Relaxed);
                }
                Err(err) => {
                    // Markup the rich parser refused: send the plain version once.
                    tracing::warn!(error = %err, "a rich message was refused, sending it as basic HTML");
                }
            }
        }
        let mut body = json!({
            "chat_id": chat_id,
            "text": naw_bots::rich_to_basic_html(html),
            "parse_mode": "HTML",
            "link_preview_options": { "is_disabled": true },
        });
        if let Some(markup) = markup {
            body["reply_markup"] = markup;
        }
        self.quick("sendMessage", &body).await.map(|_| ())
    }

    /// The next updates after `offset`, waiting up to [`POLL_SECS`].
    pub async fn updates(&self, offset: i64) -> Result<Vec<Update>, Error> {
        let body = json!({
            "offset": offset,
            "timeout": POLL_SECS,
            "allowed_updates": ["message", "callback_query"],
        });
        let result = self
            .call("getUpdates", &body, Duration::from_secs(POLL_SECS + 15))
            .await?;
        Ok(serde_json::from_value(result).unwrap_or_default())
    }

    /// The command menu for one language (`None`: the default menu).
    pub async fn set_commands(
        &self,
        commands: &[(String, String)],
        language: Option<&str>,
    ) -> Result<(), Error> {
        let list: Vec<Value> = commands
            .iter()
            .map(|(command, description)| json!({ "command": command, "description": description }))
            .collect();
        let mut body = json!({ "commands": list });
        if let Some(language) = language {
            body["language_code"] = json!(language);
        }
        self.quick("setMyCommands", &body).await.map(|_| ())
    }

    /// Ends the spinner on a pressed button, with a short note.
    pub async fn answer_callback(&self, id: &str, text: &str) -> Result<(), Error> {
        self.quick(
            "answerCallbackQuery",
            &json!({ "callback_query_id": id, "text": text }),
        )
        .await
        .map(|_| ())
    }
}

/// The command and its argument: `/start@WikiBot code` is `("/start", "code")`.
pub fn command(text: &str) -> (String, String) {
    let text = text.trim();
    let (head, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let head = head.split('@').next().unwrap_or(head).to_ascii_lowercase();
    (head, rest.trim().to_string())
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
            command("/LINK@FilianWikiBot  code "),
            ("/link".to_string(), "code".to_string())
        );
        assert_eq!(command(""), (String::new(), String::new()));
    }

    #[test]
    fn the_bot_id_is_the_token_prefix() {
        assert_eq!(Client::new("8100000000:secret").bot_id(), "8100000000");
    }

    #[test]
    fn only_https_url_buttons_go_out() {
        assert!(
            Button::Url {
                label: "x".into(),
                url: "http://localhost".into()
            }
            .json()
            .is_none()
        );
        assert!(
            Button::Callback {
                label: "ru".into(),
                data: "lang:ru".into()
            }
            .json()
            .is_some()
        );
    }
}

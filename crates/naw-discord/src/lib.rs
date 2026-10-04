//! A small Discord client for the wiki's bot.
//!
//! Slash commands reach the wiki as signed HTTP requests to its
//! interactions endpoint: no gateway connection, nothing to keep open. Each
//! request is checked against the application's public key (Ed25519 over
//! the timestamp and the body) before anything else looks at it, and an
//! answer goes back in the same response, seen only by the person who asked
//! (ephemeral). Direct messages need a server in common with the bot or the
//! app installed on the account; when Discord refuses one, that is reported
//! and nothing else happens.
//!
//! Every message goes out with mentions turned off, so no text, ours or
//! anyone's, can ping a person or a role.

use std::time::Duration;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::{Value, json};

const API: &str = "https://discord.com/api/v10";

/// Message flag: only the person who used the command sees the answer.
pub const EPHEMERAL: u64 = 1 << 6;

#[derive(Debug)]
pub enum Error {
    /// The person cannot get a direct message from the bot (no common
    /// server, or DMs closed).
    Unreachable,
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable => write!(f, "the user cannot be messaged by the bot"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

/// Whether `signature` (hex) is the application's signature over
/// `timestamp` followed by `body`.
pub fn verify(public_key_hex: &str, signature_hex: &str, timestamp: &str, body: &[u8]) -> bool {
    let Ok(key_bytes) = hex::decode(public_key_hex.trim()) else {
        return false;
    };
    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&key_array) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&sig_array);
    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);
    key.verify(&message, &signature).is_ok()
}

#[derive(Deserialize, Debug)]
pub struct Interaction {
    #[serde(rename = "type")]
    pub kind: u8,
    pub data: Option<CommandData>,
    pub user: Option<DiscordUser>,
    pub member: Option<Member>,
    /// The person's client language: `ru`, `en-US`.
    pub locale: Option<String>,
}

impl Interaction {
    /// The person who used the command, in a server or in a DM.
    pub fn author(&self) -> Option<&DiscordUser> {
        self.user
            .as_ref()
            .or_else(|| self.member.as_ref().map(|m| &m.user))
    }

    /// The command's string option `name`.
    pub fn option(&self, name: &str) -> Option<&str> {
        self.data
            .as_ref()?
            .options
            .iter()
            .find(|o| o.name == name)
            .and_then(|o| o.value.as_str())
    }
}

#[derive(Deserialize, Debug)]
pub struct CommandData {
    pub name: String,
    #[serde(default)]
    pub options: Vec<CommandOption>,
}

#[derive(Deserialize, Debug)]
pub struct CommandOption {
    pub name: String,
    #[serde(default)]
    pub value: Value,
}

#[derive(Deserialize, Debug)]
pub struct Member {
    pub user: DiscordUser,
}

#[derive(Deserialize, Debug)]
pub struct DiscordUser {
    pub id: String,
    pub username: Option<String>,
}

/// Interaction kinds Discord sends.
pub const PING: u8 = 1;
pub const COMMAND: u8 = 2;

/// The answer to a ping.
pub fn pong() -> Value {
    json!({ "type": 1 })
}

/// A link button for a message.
pub fn link_button(label: &str, url: &str) -> Value {
    json!({ "type": 2, "style": 5, "label": label, "url": url })
}

/// An answer only the asker sees, with link buttons in one row.
pub fn ephemeral(content: &str, buttons: &[Value]) -> Value {
    let mut data = json!({
        "content": content,
        "flags": EPHEMERAL,
        "allowed_mentions": { "parse": [] },
    });
    if !buttons.is_empty() {
        data["components"] = json!([{ "type": 1, "components": buttons }]);
    }
    json!({ "type": 4, "data": data })
}

pub struct Client {
    token: String,
    app_id: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(token: &str, app_id: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("DiscordBot (https://snackers.wiki, 0.1)")
            .build()
            .unwrap_or_default();
        Self {
            token: token.trim().to_string(),
            app_id: app_id.trim().to_string(),
            http,
        }
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The address that adds the app to the person's own account, so its
    /// commands work in DMs with it and anywhere they are.
    pub fn install_url(&self) -> String {
        format!(
            "https://discord.com/oauth2/authorize?client_id={}&integration_type=1&scope=applications.commands",
            self.app_id
        )
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<Value, Error> {
        let response = self
            .http
            .request(method, format!("{API}{path}"))
            .header("Authorization", format!("Bot {}", self.token))
            .json(body)
            .send()
            .await
            .map_err(|err| Error::Other(format!("{path}: {err}")))?;
        let status = response.status().as_u16();
        let answer: Value = response.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(answer);
        }
        // 50007: cannot send messages to this user.
        if status == 403 || answer.get("code").and_then(Value::as_i64) == Some(50007) {
            return Err(Error::Unreachable);
        }
        Err(Error::Other(format!(
            "{path}: {status} {}",
            answer.get("message").and_then(Value::as_str).unwrap_or("")
        )))
    }

    /// Replaces the app's global slash commands with `commands`.
    pub async fn register_commands(&self, commands: &Value) -> Result<(), Error> {
        self.request(
            reqwest::Method::PUT,
            &format!("/applications/{}/commands", self.app_id),
            commands,
        )
        .await
        .map(|_| ())
    }

    /// Sends a direct message, with link buttons in one row.
    pub async fn dm(&self, user_id: &str, content: &str, buttons: &[Value]) -> Result<(), Error> {
        let channel = self
            .request(
                reqwest::Method::POST,
                "/users/@me/channels",
                &json!({ "recipient_id": user_id }),
            )
            .await?;
        let Some(channel_id) = channel.get("id").and_then(Value::as_str) else {
            return Err(Error::Other("no DM channel id".to_string()));
        };
        let mut body = json!({ "content": content, "allowed_mentions": { "parse": [] } });
        if !buttons.is_empty() {
            body["components"] = json!([{ "type": 1, "components": buttons }]);
        }
        self.request(
            reqwest::Method::POST,
            &format!("/channels/{channel_id}/messages"),
            &body,
        )
        .await
        .map(|_| ())
    }
}

/// Discord's language for a client locale: `en-US` and `en-GB` are `en`.
pub fn language(locale: Option<&str>) -> Option<String> {
    let code = locale?.split(['-', '_']).next()?.to_ascii_lowercase();
    (!code.is_empty()).then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn only_a_signed_request_passes() {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let public = hex::encode(signing.verifying_key().to_bytes());
        let body = br#"{"type":1}"#;
        let mut message = b"1700000000".to_vec();
        message.extend_from_slice(body);
        let signature = hex::encode(signing.sign(&message).to_bytes());
        assert!(verify(&public, &signature, "1700000000", body));
        assert!(
            !verify(&public, &signature, "1700000001", body),
            "another timestamp"
        );
        assert!(
            !verify(&public, &signature, "1700000000", br#"{"type":2}"#),
            "another body"
        );
        assert!(!verify(&public, "zz", "1700000000", body), "not hex");
        assert!(!verify("00", &signature, "1700000000", body), "a short key");
    }

    #[test]
    fn answers_never_ping() {
        let answer = ephemeral("hi @everyone", &[link_button("Open", "https://x")]);
        assert_eq!(answer["data"]["allowed_mentions"]["parse"], json!([]));
        assert_eq!(answer["data"]["flags"], json!(EPHEMERAL));
        assert_eq!(
            answer["data"]["components"][0]["components"][0]["style"],
            json!(5)
        );
    }

    #[test]
    fn locales_become_languages() {
        assert_eq!(language(Some("en-US")).as_deref(), Some("en"));
        assert_eq!(language(Some("ru")).as_deref(), Some("ru"));
        assert_eq!(language(None), None);
    }

    #[test]
    fn options_are_read_by_name() {
        let interaction: Interaction = serde_json::from_value(json!({
            "type": 2,
            "data": { "name": "link", "options": [{ "name": "code", "type": 3, "value": "abc" }] },
            "member": { "user": { "id": "42", "username": "snack" } },
            "locale": "ru"
        }))
        .expect("parses");
        assert_eq!(interaction.option("code"), Some("abc"));
        assert_eq!(interaction.author().map(|u| u.id.as_str()), Some("42"));
    }
}

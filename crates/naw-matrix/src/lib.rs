//! A small Matrix client for the wiki's bot, ready for when it joins the
//! Telegram and Discord bots.
//!
//! It speaks the Client-Server API (v3) of any homeserver with an access
//! token of a bot account made there: it checks who the token is, opens a
//! direct room with a person, and sends cards (`naw_bots::card`) as notices
//! with HTML in `formatted_body` and the plain text in `body`, so every
//! client shows something. A notice is what bots send in Matrix: clients do
//! not answer it with another bot's notice, so two bots cannot loop.
//!
//! The token is sent as a header, never in the address, so no error carries
//! it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Debug)]
pub enum Error {
    /// The homeserver refused the token or the room (`M_FORBIDDEN`,
    /// `M_UNKNOWN_TOKEN`).
    Forbidden,
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forbidden => f.write_str("refused by the homeserver"),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

pub struct Client {
    homeserver: String,
    token: String,
    http: reqwest::Client,
    /// Makes each send's transaction id unique within this process.
    txn: AtomicU64,
}

impl Client {
    /// `homeserver` as `https://matrix.example`, without a trailing slash.
    pub fn new(homeserver: &str, access_token: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            homeserver: homeserver.trim().trim_end_matches('/').to_string(),
            token: access_token.trim().to_string(),
            http,
            txn: AtomicU64::new(0),
        }
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, Error> {
        let mut request = self
            .http
            .request(
                method,
                format!("{}/_matrix/client/v3{path}", self.homeserver),
            )
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| Error::Other(format!("{path}: {}", err.without_url())))?;
        let status = response.status().as_u16();
        let answer: Value = response.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(answer);
        }
        let code = answer.get("errcode").and_then(Value::as_str).unwrap_or("");
        if matches!(code, "M_FORBIDDEN" | "M_UNKNOWN_TOKEN" | "M_MISSING_TOKEN") {
            return Err(Error::Forbidden);
        }
        Err(Error::Other(format!(
            "{path}: {status} {code} {}",
            answer.get("error").and_then(Value::as_str).unwrap_or("")
        )))
    }

    /// The bot's own Matrix id, such as `@wiki:matrix.example`.
    pub async fn whoami(&self) -> Result<String, Error> {
        let answer = self
            .request(reqwest::Method::GET, "/account/whoami", None)
            .await?;
        answer
            .get("user_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Other("whoami answered without a user_id".to_string()))
    }

    /// Opens a direct room with `user_id` and invites them; the room's id.
    pub async fn open_direct(&self, user_id: &str) -> Result<String, Error> {
        let answer = self
            .request(
                reqwest::Method::POST,
                "/createRoom",
                Some(&json!({
                    "is_direct": true,
                    "invite": [user_id],
                    "preset": "trusted_private_chat",
                })),
            )
            .await?;
        answer
            .get("room_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Other("createRoom answered without a room_id".to_string()))
    }

    /// Sends a card to a room as a notice.
    pub async fn send_card(&self, room_id: &str, card: &naw_bots::card::Card) -> Result<(), Error> {
        let txn = format!(
            "naw-{}-{}",
            std::process::id(),
            self.txn.fetch_add(1, Ordering::Relaxed)
        );
        let path = format!("/rooms/{}/send/m.room.message/{txn}", encode_path(room_id));
        self.request(reqwest::Method::PUT, &path, Some(&notice(card)))
            .await
            .map(|_| ())
    }
}

/// The `m.room.message` content for a card.
pub fn notice(card: &naw_bots::card::Card) -> Value {
    let (html, plain) = card.matrix();
    json!({
        "msgtype": "m.notice",
        "body": plain,
        "format": "org.matrix.custom.html",
        "formatted_body": html,
    })
}

/// A room id or alias as one path segment: `!abc:server` keeps its letters,
/// everything else is percent-encoded.
fn encode_path(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len() + 8);
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use naw_bots::card::{Button, Card, Rich, Tone};

    #[test]
    fn a_card_is_a_notice_with_html_and_a_plain_twin() {
        let card = Card::new(Tone::Info)
            .heading(Rich::plain("Filian <Wiki>"))
            .text(Rich::new("Code: <code>{code}</code>").arg("code", "123456"))
            .row(vec![Button::url("Open", "https://filian.wiki/")]);
        let content = notice(&card);
        assert_eq!(content["msgtype"], "m.notice");
        assert_eq!(content["format"], "org.matrix.custom.html");
        let html = content["formatted_body"].as_str().unwrap_or("");
        assert!(html.contains("<h3>Filian &lt;Wiki&gt;</h3>"), "{html}");
        assert!(html.contains("<code>123456</code>"));
        let body = content["body"].as_str().unwrap_or("");
        assert!(body.starts_with("Filian <Wiki>"));
        assert!(body.contains("Open: https://filian.wiki/"));
    }

    #[test]
    fn a_room_id_is_one_path_segment() {
        assert_eq!(encode_path("!abc:matrix.org"), "%21abc%3Amatrix.org");
        assert_eq!(encode_path("#a/b"), "%23a%2Fb");
    }
}

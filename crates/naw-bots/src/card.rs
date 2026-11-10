//! A bot message built once, from parts, and drawn for each chat platform as
//! richly as that platform takes it.
//!
//! A [`Card`] has a tone (Discord shows it as the colour of the bar on the
//! left), blocks of text, and rows of buttons. Every piece of text is a
//! [`Rich`]: a trusted locale template in Telegram's rich HTML with its
//! arguments kept apart, so each platform escapes them its own way (see the
//! crate's [`crate::render`]). Nothing a person typed can format or link.
//!
//! Each platform has a rich drawing and a plain one, and a client falls back
//! to the plain one when the rich one is refused:
//!
//! - Telegram: a rich message (headings, lists, `<details>`, quotes) with the
//!   buttons under it in their styles; else its basic HTML.
//! - Discord: Components V2, a container with an accent colour, text
//!   displays and separators, and the buttons in its styles; else Markdown
//!   content with plain buttons.
//! - Matrix: HTML as its `formatted_body`, and the plain text as `body`.

use serde_json::{Value, json};

use crate::{Platform, escape_telegram, render};

/// The feeling of a message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    /// Discord's accent colour for the tone, as an RGB number.
    pub fn colour(self) -> Option<u32> {
        match self {
            Self::Neutral => None,
            Self::Info => Some(0x4f7cff),
            Self::Success => Some(0x2e9e5b),
            Self::Warning => Some(0xe0a020),
            Self::Danger => Some(0xd64545),
        }
    }
}

/// Text with its arguments: a trusted template and the values that go in,
/// escaped for whichever platform draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rich {
    template: String,
    args: Vec<(String, String)>,
}

impl Rich {
    /// A trusted template, such as a locale text.
    pub fn new(template: impl Into<String>) -> Self {
        Self {
            template: template.into(),
            args: Vec::new(),
        }
    }

    /// Untrusted text shown as it is: a page title, a name, a note.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::new("{text}").arg("text", text)
    }

    /// Untrusted text in a trusted wrapper, such as `<b>{text}</b>`.
    pub fn wrapped(template: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(template).arg("text", text)
    }

    pub fn arg(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.push((name.into(), value.into()));
        self
    }

    /// The text for `platform`, its arguments escaped there.
    pub fn to(&self, platform: Platform) -> String {
        let args: Vec<(&str, &str)> = self
            .args
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        render(&self.template, &args, platform)
    }

    /// The text with no markup at all, for a notification preview or a log.
    pub fn plain_text(&self) -> String {
        strip_tags(&self.to(Platform::Matrix))
    }
}

/// One part of a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(Rich),
    Text(Rich),
    /// Label and value pairs, one per line.
    Fields(Vec<(Rich, Rich)>),
    List(Vec<Rich>),
    Quote(Rich),
    /// A summary that opens to show more, where the platform can.
    Details {
        summary: Rich,
        body: Rich,
    },
    Divider,
    /// Small print at the bottom.
    Footer(Rich),
}

/// How a button looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Plain,
    Primary,
    Success,
    Danger,
}

/// What a button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Opens an address. Only public HTTPS survives on every platform.
    Url(String),
    /// Comes back to the bot as `data`, at most 64 bytes.
    Callback(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub action: Action,
    pub style: Style,
}

impl Button {
    pub fn url(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: Action::Url(url.into()),
            style: Style::Plain,
        }
    }

    pub fn callback(label: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: Action::Callback(data.into()),
            style: Style::Plain,
        }
    }

    pub fn styled(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Whether every platform takes this button: a public HTTPS address,
    /// or callback data of 1 to 64 bytes.
    fn sendable(&self) -> bool {
        match &self.action {
            Action::Url(url) => url.starts_with("https://"),
            Action::Callback(data) => (1..=64).contains(&data.len()),
        }
    }
}

/// A whole message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Card {
    pub tone: Tone,
    pub blocks: Vec<Block>,
    pub buttons: Vec<Vec<Button>>,
}

impl Card {
    pub fn new(tone: Tone) -> Self {
        Self {
            tone,
            ..Self::default()
        }
    }

    pub fn block(mut self, block: Block) -> Self {
        self.blocks.push(block);
        self
    }

    pub fn heading(self, text: Rich) -> Self {
        self.block(Block::Heading(text))
    }

    pub fn text(self, text: Rich) -> Self {
        self.block(Block::Text(text))
    }

    pub fn footer(self, text: Rich) -> Self {
        self.block(Block::Footer(text))
    }

    /// A row of buttons; the ones no platform takes are left out.
    pub fn row(mut self, buttons: Vec<Button>) -> Self {
        let row: Vec<Button> = buttons.into_iter().filter(Button::sendable).collect();
        if !row.is_empty() {
            self.buttons.push(row);
        }
        self
    }

    /// Telegram's rich HTML (`sendRichMessage`).
    pub fn telegram_rich(&self) -> String {
        let p = Platform::Telegram;
        let mut out = String::new();
        for block in &self.blocks {
            match block {
                Block::Heading(t) => out.push_str(&format!("<h3>{}</h3>", t.to(p))),
                Block::Text(t) => out.push_str(&format!("<p>{}</p>", t.to(p))),
                Block::Fields(fields) => {
                    for (label, value) in fields {
                        out.push_str(&format!("<p><b>{}</b> {}</p>", label.to(p), value.to(p)));
                    }
                }
                Block::List(items) => {
                    out.push_str("<ul>");
                    for item in items {
                        out.push_str(&format!("<li>{}</li>", item.to(p)));
                    }
                    out.push_str("</ul>");
                }
                Block::Quote(t) => out.push_str(&format!("<blockquote>{}</blockquote>", t.to(p))),
                Block::Details { summary, body } => out.push_str(&format!(
                    "<details><summary>{}</summary><p>{}</p></details>",
                    summary.to(p),
                    body.to(p)
                )),
                Block::Divider => out.push_str("<hr/>"),
                Block::Footer(t) => out.push_str(&format!("<footer>{}</footer>", t.to(p))),
            }
        }
        out
    }

    /// Telegram's basic HTML (`sendMessage`), which knows only inline tags.
    pub fn telegram_basic(&self) -> String {
        let p = Platform::Telegram;
        let mut lines: Vec<String> = Vec::new();
        for block in &self.blocks {
            let line = match block {
                Block::Heading(t) => format!("<b>{}</b>", t.to(p)),
                Block::Text(t) => t.to(p),
                Block::Fields(fields) => fields
                    .iter()
                    .map(|(l, v)| format!("<b>{}</b> {}", l.to(p), v.to(p)))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Block::List(items) => items
                    .iter()
                    .map(|i| format!("• {}", i.to(p)))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Block::Quote(t) => format!("<blockquote>{}</blockquote>", t.to(p)),
                Block::Details { summary, body } => format!(
                    "<b>{}</b>\n<blockquote expandable>{}</blockquote>",
                    summary.to(p),
                    body.to(p)
                ),
                Block::Divider => "──────────".to_string(),
                Block::Footer(t) => format!("<i>{}</i>", t.to(p)),
            };
            lines.push(crate::rich_to_basic_html(&line).trim_end().to_string());
        }
        lines.join("\n\n")
    }

    /// Telegram's inline keyboard for the buttons, with their styles.
    pub fn telegram_keyboard(&self) -> Option<Value> {
        let rows: Vec<Vec<Value>> = self
            .buttons
            .iter()
            .map(|row| row.iter().map(telegram_button).collect())
            .collect();
        (!rows.is_empty()).then(|| json!({ "inline_keyboard": rows }))
    }

    /// Discord's Markdown for the blocks.
    pub fn discord_markdown(&self) -> String {
        let p = Platform::Discord;
        let mut parts: Vec<String> = Vec::new();
        for block in &self.blocks {
            parts.push(match block {
                Block::Heading(t) => format!("### {}", t.to(p).trim()),
                Block::Text(t) => t.to(p),
                Block::Fields(fields) => fields
                    .iter()
                    .map(|(l, v)| format!("**{}** {}", l.to(p), v.to(p)))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Block::List(items) => items
                    .iter()
                    .map(|i| format!("- {}", i.to(p)))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Block::Quote(t) => quoted(&t.to(p)),
                Block::Details { summary, body } => {
                    format!("**{}**\n{}", summary.to(p), quoted(&body.to(p)))
                }
                Block::Divider => "───".to_string(),
                Block::Footer(t) => format!("-# {}", t.to(p).trim()),
            });
        }
        parts.join("\n\n")
    }

    /// Discord Components V2: a container in the tone's colour with the
    /// blocks as text displays and separators, then the buttons. Sent with
    /// the flag [`DISCORD_COMPONENTS_V2`] and no `content`.
    pub fn discord_components(&self) -> Value {
        let p = Platform::Discord;
        let mut inner: Vec<Value> = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, inner: &mut Vec<Value>| {
            if !text.trim().is_empty() {
                inner.push(json!({ "type": 10, "content": text.trim() }));
            }
            text.clear();
        };
        for block in &self.blocks {
            match block {
                Block::Divider => {
                    flush(&mut text, &mut inner);
                    inner.push(json!({ "type": 14, "divider": true, "spacing": 1 }));
                }
                Block::Footer(t) => {
                    flush(&mut text, &mut inner);
                    inner.push(json!({ "type": 14, "divider": false, "spacing": 1 }));
                    text.push_str(&format!("-# {}", t.to(p).trim()));
                }
                other => {
                    let one = Card {
                        blocks: vec![other.clone()],
                        ..Card::default()
                    };
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&one.discord_markdown());
                }
            }
        }
        flush(&mut text, &mut inner);
        let mut container = json!({ "type": 17, "components": inner });
        if let Some(colour) = self.tone.colour() {
            container["accent_color"] = json!(colour);
        }
        let mut out = vec![container];
        out.extend(self.discord_rows());
        Value::Array(out)
    }

    /// The buttons as Discord action rows, at most five to a row.
    pub fn discord_rows(&self) -> Vec<Value> {
        self.buttons
            .iter()
            .flat_map(|row| row.chunks(5))
            .map(|row| json!({ "type": 1, "components": row.iter().map(discord_button).collect::<Vec<_>>() }))
            .collect()
    }

    /// Matrix's `formatted_body` (HTML) and `body` (plain text).
    pub fn matrix(&self) -> (String, String) {
        let p = Platform::Matrix;
        let mut html = String::new();
        let mut plain: Vec<String> = Vec::new();
        for block in &self.blocks {
            let (h, t) = match block {
                Block::Heading(t) => (format!("<h3>{}</h3>", t.to(p)), t.plain_text()),
                Block::Text(t) => (format!("<p>{}</p>", t.to(p)), t.plain_text()),
                Block::Fields(fields) => (
                    fields
                        .iter()
                        .map(|(l, v)| format!("<p><b>{}</b> {}</p>", l.to(p), v.to(p)))
                        .collect(),
                    fields
                        .iter()
                        .map(|(l, v)| format!("{} {}", l.plain_text(), v.plain_text()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Block::List(items) => (
                    format!(
                        "<ul>{}</ul>",
                        items
                            .iter()
                            .map(|i| format!("<li>{}</li>", i.to(p)))
                            .collect::<String>()
                    ),
                    items
                        .iter()
                        .map(|i| format!("- {}", i.plain_text()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Block::Quote(t) => (
                    format!("<blockquote>{}</blockquote>", t.to(p)),
                    format!("> {}", t.plain_text()),
                ),
                Block::Details { summary, body } => (
                    format!(
                        "<details><summary>{}</summary>{}</details>",
                        summary.to(p),
                        body.to(p)
                    ),
                    format!("{}\n{}", summary.plain_text(), body.plain_text()),
                ),
                Block::Divider => ("<hr>".to_string(), "---".to_string()),
                Block::Footer(t) => (format!("<p><sub>{}</sub></p>", t.to(p)), t.plain_text()),
            };
            html.push_str(&h);
            plain.push(t);
        }
        // Matrix has no buttons in plain messages: links go under the text.
        let links: Vec<(String, String)> = self
            .buttons
            .iter()
            .flatten()
            .filter_map(|b| match &b.action {
                Action::Url(url) => Some((b.label.clone(), url.clone())),
                Action::Callback(_) => None,
            })
            .collect();
        if !links.is_empty() {
            html.push_str("<p>");
            html.push_str(
                &links
                    .iter()
                    .map(|(label, url)| {
                        format!(
                            "<a href=\"{}\">{}</a>",
                            escape_telegram(url),
                            escape_telegram(label)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" · "),
            );
            html.push_str("</p>");
            plain.extend(links.iter().map(|(label, url)| format!("{label}: {url}")));
        }
        (html, plain.join("\n\n"))
    }
}

/// Components V2 on a Discord message: `flags` with this bit set.
pub const DISCORD_COMPONENTS_V2: u64 = 1 << 15;

fn quoted(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn telegram_button(button: &Button) -> Value {
    let mut out = match &button.action {
        Action::Url(url) => json!({ "text": button.label, "url": url }),
        Action::Callback(data) => json!({ "text": button.label, "callback_data": data }),
    };
    let style = match button.style {
        Style::Plain => None,
        Style::Primary => Some("primary"),
        Style::Success => Some("success"),
        Style::Danger => Some("danger"),
    };
    if let Some(style) = style {
        out["style"] = json!(style);
    }
    out
}

fn discord_button(button: &Button) -> Value {
    let label: String = button.label.chars().take(80).collect();
    match &button.action {
        // A link button is always grey on Discord.
        Action::Url(url) => json!({ "type": 2, "style": 5, "label": label, "url": url }),
        Action::Callback(data) => {
            let style = match button.style {
                Style::Primary => 1,
                Style::Plain => 2,
                Style::Success => 3,
                Style::Danger => 4,
            };
            json!({ "type": 2, "style": style, "label": label, "custom_id": data })
        }
    }
}

/// HTML without its tags and with its entities as characters.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    crate::decode_entities(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Card {
        Card::new(Tone::Warning)
            .heading(Rich::wrapped("{text}", "Filian <Wiki>"))
            .text(Rich::new("New sign-in to <b>{user}</b>.").arg("user", "*bold*"))
            .block(Block::Fields(vec![(
                Rich::new("Device:"),
                Rich::plain("Firefox, Linux"),
            )]))
            .block(Block::Divider)
            .footer(Rich::new("<i>Not you? Change the password.</i>"))
            .row(vec![
                Button::url("Open settings", "https://filian.wiki/settings").styled(Style::Primary),
                Button::url("Local only", "http://localhost/x"),
                Button::callback("Sign out everywhere", "out:all").styled(Style::Danger),
            ])
    }

    #[test]
    fn telegram_gets_rich_blocks_and_styled_buttons() {
        let card = sample();
        let html = card.telegram_rich();
        assert!(html.starts_with("<h3>Filian &lt;Wiki&gt;</h3>"), "{html}");
        assert!(html.contains("<b>*bold*</b>"));
        assert!(html.contains("<hr/>") && html.contains("<footer>"));
        let keyboard = card.telegram_keyboard().expect("buttons");
        let row = &keyboard["inline_keyboard"][0];
        assert_eq!(
            row.as_array().map(Vec::len),
            Some(2),
            "the http button is dropped"
        );
        assert_eq!(row[0]["style"], "primary");
        assert_eq!(row[1]["style"], "danger");
        assert_eq!(row[1]["callback_data"], "out:all");
    }

    #[test]
    fn telegram_basic_has_only_inline_tags() {
        let basic = sample().telegram_basic();
        for tag in ["<h3", "<p>", "<footer", "<hr", "<ul", "<details"] {
            assert!(!basic.contains(tag), "{tag} in {basic}");
        }
        assert!(basic.starts_with("<b>Filian &lt;Wiki&gt;</b>"));
    }

    #[test]
    fn discord_gets_a_coloured_container_and_escaped_markdown() {
        let card = sample();
        let components = card.discord_components();
        let container = &components[0];
        assert_eq!(container["type"], 17);
        assert_eq!(container["accent_color"], 0xe0a020);
        let text = container["components"][0]["content"].as_str().unwrap_or("");
        assert!(
            text.starts_with("### Filian <Wiki>") || text.starts_with("### Filian \\<Wiki"),
            "{text}"
        );
        assert!(text.contains("**\\*bold\\***"), "{text}");
        let row = &components[1];
        assert_eq!(row["type"], 1);
        assert_eq!(row["components"][1]["style"], 4, "danger");
        assert_eq!(row["components"][0]["style"], 5, "a link");
    }

    #[test]
    fn matrix_gets_html_and_a_plain_twin() {
        let (html, plain) = sample().matrix();
        assert!(html.contains("<h3>Filian &lt;Wiki&gt;</h3>"));
        assert!(html.contains("<a href=\"https://filian.wiki/settings\">Open settings</a>"));
        assert!(plain.starts_with("Filian <Wiki>"));
        assert!(plain.contains("New sign-in to *bold*."));
        assert!(!plain.contains('<') || plain.contains("Filian <Wiki>"));
    }

    #[test]
    fn a_neutral_card_has_no_colour() {
        let card = Card::new(Tone::Neutral).text(Rich::plain("hi"));
        assert!(card.discord_components()[0].get("accent_color").is_none());
        assert!(card.telegram_keyboard().is_none());
    }
}

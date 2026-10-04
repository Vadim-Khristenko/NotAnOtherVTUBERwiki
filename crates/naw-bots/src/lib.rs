//! Bot messages, written once and made safe for each chat platform.
//!
//! A bot text lives in the locale files in Telegram's rich HTML: `<b>`,
//! `<i>`, `<u>`, `<s>`, `<code>`, `<pre>`, `<a href>`, `<blockquote>`,
//! `<tg-spoiler>`, `<h1>` to `<h3>`, `<p>`, `<br>`. It is trusted text, so
//! its tags stay tags. Everything put into it with `{name}` is not: a
//! username, a page title or a note goes in escaped for the platform the
//! message goes to, so nobody can write formatting, a link or a mention
//! into a bot's message by naming themselves cleverly.
//!
//! Telegram gets the HTML as is. Discord gets it turned into its Markdown,
//! with the arguments escaped for Markdown and `@everyone` and the like
//! made harmless. A premium (custom) emoji is written `{emoji:ID|👍}`: a
//! `<tg-emoji>` on Telegram, which shows the custom emoji where the bot may
//! send one and the fallback elsewhere, and just the fallback on Discord.
//!
//! An argument whose name ends in `url` is an address: it is escaped for
//! HTML on Telegram but never backslashed on Discord, where that would
//! break the link.

/// Where a message goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Telegram,
    Discord,
}

/// Text for Telegram's HTML: the three characters that matter, and quotes
/// for attribute values.
pub fn escape_telegram(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Text for Discord's Markdown: every formatting character backslashed, and
/// mentions broken with a zero-width space so `@everyone` pings nobody.
pub fn escape_discord(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '(' | ')' | '-' | '<' => {
                out.push('\\');
                out.push(c);
            }
            '@' => out.push_str("@\u{200b}"),
            c => out.push(c),
        }
    }
    out
}

/// An address for Discord: left as it is, except what would end the link.
fn discord_url(url: &str) -> String {
    url.replace(')', "%29")
        .replace(' ', "%20")
        .replace('<', "%3C")
        .replace('>', "%3E")
}

fn is_url(name: &str) -> bool {
    name.ends_with("url")
}

/// The custom emoji written `{emoji:ID|fallback}`, for `platform`. An id
/// that is not all digits keeps only the fallback.
fn emojis(template: &str, platform: Platform) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{emoji:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 7..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let body = &after[..end];
        let (id, fallback) = body.split_once('|').unwrap_or((body, ""));
        match platform {
            Platform::Telegram if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                out.push_str(&format!(
                    "<tg-emoji emoji-id=\"{id}\">{}</tg-emoji>",
                    escape_telegram(fallback)
                ));
            }
            _ => out.push_str(fallback),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Puts the arguments into `template` for `platform`, escaped.
fn fill_args(template: &str, args: &[(&str, &str)], platform: Platform) -> String {
    let mut out = String::with_capacity(template.len() + 64);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('}');
        let name = end.map(|e| &after[..e]);
        match name.and_then(|n| args.iter().find(|(k, _)| *k == n).map(|(k, v)| (*k, *v))) {
            Some((key, value)) => {
                let safe = match platform {
                    Platform::Telegram => escape_telegram(value),
                    Platform::Discord if is_url(key) => discord_url(value),
                    Platform::Discord => escape_discord(value),
                };
                out.push_str(&safe);
                rest = &after[end.unwrap_or(0) + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The entities a locale file may write, as characters.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", "\u{a0}")
        .replace("&hellip;", "\u{2026}")
        .replace("&amp;", "&")
}

/// The value of `name="..."` in a tag's attributes.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(decode_entities(&tag[start..start + end]))
}

/// The rich HTML of a locale text as Discord Markdown. Runs on the trusted
/// template before any argument is in it.
pub fn html_to_discord(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut links: Vec<String> = Vec::new();
    let mut quote = false;
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        push_text(&mut out, &rest[..open], quote);
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            push_text(&mut out, &rest[open..], quote);
            rest = "";
            break;
        };
        let tag = &after[..close];
        rest = &after[close + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("b" | "strong", _) => out.push_str("**"),
            ("i" | "em", _) => out.push('*'),
            ("u" | "ins", _) => out.push_str("__"),
            ("s" | "strike" | "del", _) => out.push_str("~~"),
            ("code", _) => out.push('`'),
            ("pre", false) => out.push_str("```\n"),
            ("pre", true) => out.push_str("\n```"),
            ("tg-spoiler", _) => out.push_str("||"),
            ("br", _) => {
                out.push('\n');
                if quote {
                    out.push_str("> ");
                }
            }
            ("h1", false) => out.push_str("# "),
            ("h2", false) => out.push_str("## "),
            ("h3" | "h4" | "h5" | "h6", false) => out.push_str("### "),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p", true) => out.push('\n'),
            ("li", false) => out.push_str("- "),
            ("li", true) => out.push('\n'),
            ("blockquote" | "aside", false) => {
                quote = true;
                out.push_str("> ");
            }
            ("blockquote" | "aside", true) => {
                quote = false;
                out.push('\n');
            }
            ("a", false) => {
                links.push(attribute(tag, "href").unwrap_or_default());
                out.push('[');
            }
            ("a", true) => {
                let href = links.pop().unwrap_or_default();
                out.push_str(&format!("]({href})"));
            }
            // Anything else (footer, lists, custom tags) leaves only its text.
            _ => {}
        }
    }
    push_text(&mut out, rest, quote);
    out
}

fn push_text(out: &mut String, text: &str, quote: bool) {
    let text = decode_entities(text);
    if quote {
        out.push_str(&text.replace('\n', "\n> "));
    } else {
        out.push_str(&text);
    }
}

/// A locale text with its arguments, ready to send on `platform`.
pub fn render(template: &str, args: &[(&str, &str)], platform: Platform) -> String {
    let with_emoji = emojis(template, platform);
    match platform {
        Platform::Telegram => fill_args(&with_emoji, args, platform),
        Platform::Discord => fill_args(&html_to_discord(&with_emoji), args, platform),
    }
}

/// Telegram's plain HTML mode (sendMessage) knows fewer tags than its rich
/// messages: headings and paragraphs become bold lines and line breaks.
pub fn rich_to_basic_html(html: &str) -> String {
    // A custom emoji may be what the rich send was refused for (the bot's
    // owner has no Premium): the basic send keeps only its fallback.
    let mut out = strip_custom_emoji(html);
    for level in 1..=6 {
        out = out
            .replace(&format!("<h{level}>"), "<b>")
            .replace(&format!("</h{level}>"), "</b>\n");
    }
    out.replace("<p>", "")
        .replace("</p>", "\n")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<footer>", "<i>")
        .replace("</footer>", "</i>")
}

/// `<tg-emoji emoji-id="...">👍</tg-emoji>` as just `👍`.
fn strip_custom_emoji(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<tg-emoji") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(open_end) = after.find('>') else {
            out.push_str(after);
            return out;
        };
        let inner = &after[open_end + 1..];
        match inner.find("</tg-emoji>") {
            Some(close) => {
                out.push_str(&inner[..close]);
                rest = &inner[close + "</tg-emoji>".len()..];
            }
            None => {
                out.push_str(inner);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_html_keeps_only_the_emoji_fallback() {
        assert_eq!(
            rich_to_basic_html("a <tg-emoji emoji-id=\"1\">👍</tg-emoji> b"),
            "a 👍 b"
        );
    }

    #[test]
    fn a_name_cannot_format_or_link_on_telegram() {
        let out = render(
            "<b>{wiki}</b>\nHi <b>{user}</b>",
            &[("wiki", "Wiki"), ("user", "<a href=\"x\">me</a> & co")],
            Platform::Telegram,
        );
        assert_eq!(
            out,
            "<b>Wiki</b>\nHi <b>&lt;a href=&quot;x&quot;&gt;me&lt;/a&gt; &amp; co</b>"
        );
    }

    #[test]
    fn a_name_cannot_format_or_ping_on_discord() {
        let out = render(
            "Hi <b>{user}</b>",
            &[("user", "**@everyone** _x_")],
            Platform::Discord,
        );
        assert_eq!(out, "Hi **\\*\\*@\u{200b}everyone\\*\\* \\_x\\_**");
    }

    #[test]
    fn addresses_keep_working_on_discord() {
        let out = render(
            "Open:\n{url}",
            &[("url", "https://wiki.example/password/reset?token=ab_cd-ef")],
            Platform::Discord,
        );
        assert_eq!(
            out,
            "Open:\nhttps://wiki.example/password/reset?token=ab_cd-ef"
        );
        let tg = render(
            "{url}",
            &[("url", "https://x/?a=1&b=2")],
            Platform::Telegram,
        );
        assert_eq!(tg, "https://x/?a=1&amp;b=2");
    }

    #[test]
    fn premium_emoji_has_a_fallback() {
        let t = "{emoji:5368324170671202286|👍} done";
        assert_eq!(
            render(t, &[], Platform::Telegram),
            "<tg-emoji emoji-id=\"5368324170671202286\">👍</tg-emoji> done"
        );
        assert_eq!(render(t, &[], Platform::Discord), "👍 done");
        assert_eq!(
            render("{emoji:x1|🙂}", &[], Platform::Telegram),
            "🙂",
            "a bad id keeps the fallback"
        );
    }

    #[test]
    fn rich_html_becomes_discord_markdown() {
        let html = "<h2>Title</h2><i>soft</i> <code>c</code> <tg-spoiler>s</tg-spoiler>\n<blockquote>one<br>two</blockquote><a href=\"https://x\">link</a> &amp; more";
        assert_eq!(
            html_to_discord(html),
            "## Title\n*soft* `c` ||s||\n> one\n> two\n[link](https://x) & more"
        );
    }

    #[test]
    fn unknown_placeholders_stay() {
        assert_eq!(
            render("a {missing} b", &[], Platform::Telegram),
            "a {missing} b"
        );
    }

    #[test]
    fn basic_html_has_no_rich_only_tags() {
        assert_eq!(
            rich_to_basic_html("<h1>Hi</h1><p>text</p>"),
            "<b>Hi</b>\ntext\n"
        );
    }
}

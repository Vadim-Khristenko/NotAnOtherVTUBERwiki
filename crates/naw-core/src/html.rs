//! HTML helpers for markup written by hand.

/// Escapes text for HTML element content and quoted attribute values.
pub fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Whole mebibytes in `bytes`, never less than one, for limits shown to people.
pub fn mib(bytes: usize) -> usize {
    (bytes / (1024 * 1024)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_every_character_that_can_open_markup() {
        assert_eq!(
            escape(r#"<a href="x" title='y'>&</a>"#),
            "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;&amp;&lt;/a&gt;"
        );
        assert_eq!(escape("Филиан"), "Филиан");
    }

    #[test]
    fn mebibytes_round_down_but_never_to_zero() {
        assert_eq!(mib(20 * 1024 * 1024), 20);
        assert_eq!(mib(64 * 1024), 1);
    }
}

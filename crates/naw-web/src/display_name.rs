//! Display names: any script, minus characters that break layout or hide.
//!
//! Removed: control characters and line separators; bidi overrides,
//! isolates and marks (names render inside `<bdi>`); invisible characters,
//! except ZWJ and ZWNJ, which scripts and emoji need; and combining marks
//! past `MAX_GRAPHEME_CHARS` per grapheme, against zalgo. Whitespace runs
//! become one space.

use unicode_segmentation::UnicodeSegmentation;

/// Longest display name, in graphemes.
pub const MAX_LEN: usize = 48;
/// Code points one grapheme may carry; a family emoji is seven.
const MAX_GRAPHEME_CHARS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    TooLong,
    /// Nothing visible was left.
    Invisible,
}

impl Problem {
    pub fn key(self) -> &'static str {
        match self {
            Self::TooLong => "display_name_too_long",
            Self::Invisible => "display_name_invisible",
        }
    }
}

/// Characters removed outright.
fn is_dropped(c: char) -> bool {
    let cp = c as u32;
    // ZWJ and ZWNJ are real text.
    if matches!(cp, 0x200C | 0x200D) {
        return false;
    }
    c.is_control()
        || matches!(cp,
            0x00AD                // soft hyphen
            | 0x061C              // Arabic letter mark
            | 0x115F | 0x1160     // Hangul fillers, render as nothing
            | 0x180E              // Mongolian vowel separator
            | 0x200B              // zero width space
            | 0x200E | 0x200F     // LRM, RLM
            | 0x2028 | 0x2029     // line and paragraph separators
            | 0x202A..=0x202E     // bidi embeddings and overrides
            | 0x2060..=0x2064     // word joiner, invisible operators
            | 0x2066..=0x2069     // bidi isolates
            | 0x206A..=0x206F     // deprecated format characters
            | 0x3164              // Hangul filler
            | 0xFEFF              // byte order mark
            | 0xFFA0              // halfwidth Hangul filler
            | 0xFFF9..=0xFFFB     // interlinear annotation
            | 0xE0000..=0xE007F   // tag characters
        )
}

/// Cleans a display name; `Ok(None)` means none, show the username.
pub fn clean(input: &str) -> Result<Option<String>, Problem> {
    let kept: String = input
        .chars()
        // Before control characters go, so a tab still separates words.
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !is_dropped(*c))
        .collect();
    let mut out = String::with_capacity(kept.len());
    let mut graphemes = 0;
    let mut last_space = true; // also trims leading spaces
    for grapheme in kept.graphemes(true) {
        if grapheme == " " {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
            continue;
        }
        graphemes += 1;
        if graphemes > MAX_LEN {
            return Err(Problem::TooLong);
        }
        out.extend(grapheme.chars().take(MAX_GRAPHEME_CHARS));
        last_space = false;
    }
    let trimmed = out.trim_end().to_string();
    if trimmed.is_empty() {
        // Only spaces clears the name; losing every character was an invisible name.
        return if input.trim().is_empty() {
            Ok(None)
        } else {
            Err(Problem::Invisible)
        };
    }
    Ok(Some(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_in_any_script_pass_untouched() {
        for name in [
            "VAI (Dev Snack)",
            "Маша",
            "محمد",
            "שרה",
            "प्रिया",
            "田中",
            "🍓 Berry",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦 family",
        ] {
            assert_eq!(clean(name), Ok(Some(name.to_string())), "{name}");
        }
    }

    #[test]
    fn layout_breakers_are_removed() {
        assert_eq!(
            clean("Evil\u{202e}gnp.exe"),
            Ok(Some("Evilgnp.exe".to_string()))
        );
        assert_eq!(clean("a\u{200b}b\u{feff}c"), Ok(Some("abc".to_string())));
        assert_eq!(
            clean("line\nbreak\ttab"),
            Ok(Some("line break tab".to_string()))
        );
        assert_eq!(
            clean("  lots   of\u{3000}space  "),
            Ok(Some("lots of space".to_string()))
        );
    }

    #[test]
    fn zalgo_is_cut_down_but_diacritics_survive() {
        let zalgo = format!("Z{}", "\u{0301}".repeat(60));
        let cleaned = clean(&zalgo).unwrap().unwrap();
        assert_eq!(cleaned.chars().count(), MAX_GRAPHEME_CHARS);
        let vietnamese = "Nguyễn";
        assert_eq!(clean(vietnamese), Ok(Some(vietnamese.to_string())));
    }

    #[test]
    fn empty_clears_and_invisible_is_refused() {
        assert_eq!(clean("   "), Ok(None));
        assert_eq!(clean(""), Ok(None));
        assert_eq!(clean("\u{200b}\u{2060}"), Err(Problem::Invisible));
        assert_eq!(clean(&"a".repeat(MAX_LEN)), Ok(Some("a".repeat(MAX_LEN))));
        assert_eq!(clean(&"a".repeat(MAX_LEN + 1)), Err(Problem::TooLong));
    }
}

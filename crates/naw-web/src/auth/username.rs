//! Username policy: 3 to 32 characters of `a-z0-9_-.`, starting with a
//! letter, with dots only between other characters. Reserved names are
//! admin-only; collisions get `-2`, `-3`.

use naw_core::error::AppError;

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 32;

/// The full rule set.
pub fn is_valid(username: &str) -> bool {
    let len = username.chars().count();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&len) {
        return false;
    }
    let mut chars = username.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_lowercase() {
        return false;
    }
    if username.ends_with('.') || username.contains("..") {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

fn in_charset(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// A candidate from a provider handle: lowercase, cut at the first character
/// outside the charset (so `user@example.test` becomes `user`), capped at 32.
pub fn sanitize(handle: &str) -> String {
    handle
        .trim()
        .chars()
        .take_while(|c| in_charset(*c))
        .map(|c| c.to_ascii_lowercase())
        .collect::<String>()
        .trim_start_matches(|c: char| !c.is_ascii_lowercase())
        .chars()
        .take(USERNAME_MAX)
        .collect::<String>()
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

/// Six lowercase hex characters.
fn short_random_suffix() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 3];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// `stem-round`, trimmed to fit 32 characters.
fn with_suffix(stem: &str, round: u32) -> String {
    let suffix = format!("-{round}");
    let keep = USERNAME_MAX.saturating_sub(suffix.chars().count());
    let head: String = stem.chars().take(keep).collect();
    format!("{head}{suffix}")
}

/// Claims a free username derived from `base`, skipping reserved names,
/// taken names and aliases, on the caller's connection so the check and the
/// insert share a transaction.
pub async fn claim(
    db: &mut sqlx::PgConnection,
    reserved: &[String],
    base: &str,
) -> Result<String, AppError> {
    let mut stem = sanitize(base);
    if stem.chars().count() < USERNAME_MIN {
        stem = format!("user-{}", short_random_suffix());
    }
    let mut candidate = stem.clone();
    for round in 2..=100 {
        let reserved_hit = reserved
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&candidate));
        if !reserved_hit {
            let taken = sqlx::query!(
                "SELECT 1 AS one FROM users WHERE username = $1
                 UNION ALL
                 SELECT 1 AS one FROM user_aliases WHERE alias = $1",
                candidate
            )
            .fetch_optional(&mut *db)
            .await?
            .is_some();
            if !taken {
                return Ok(candidate);
            }
        }
        candidate = with_suffix(&stem, round);
    }
    Err(AppError::Config("username space exhausted".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_are_allowed_between_characters() {
        assert!(is_valid("just.call.me.l"));
        assert!(is_valid("a.b"));
        assert!(!is_valid(".dot"), "starts with a letter");
        assert!(!is_valid("dot."), "never last");
        assert!(!is_valid("do..t"), "never two in a row");
        assert_eq!(sanitize("Just.Call.Me.L"), "just.call.me.l");
        assert_eq!(sanitize("tail.."), "tail");
        assert_eq!(sanitize("a..b"), "a.b");
        assert_eq!(sanitize("first.last@example.test"), "first.last");
    }

    #[test]
    fn sanitize_keeps_the_charset_and_lowercases() {
        assert_eq!(sanitize("Filian-Fan_99"), "filian-fan_99");
        assert_eq!(sanitize("user@example.test"), "user");
        assert_eq!(sanitize("  spaced  "), "spaced");
        assert_eq!(sanitize("99numbers-first"), "numbers-first");
        assert_eq!(sanitize("_-__"), "");
    }

    #[test]
    fn sanitize_caps_at_32() {
        let long = "a".repeat(50);
        assert_eq!(sanitize(&long).len(), 32);
    }

    #[test]
    fn validity_enforces_the_shape() {
        assert!(is_valid("filian"));
        assert!(is_valid("a1_b-c"));
        assert!(!is_valid("ab"));
        assert!(!is_valid("1abc"));
        assert!(!is_valid("-abc"));
        assert!(!is_valid("Has-Upper"));
        assert!(!is_valid("has space"));
        assert!(!is_valid(&"a".repeat(33)));
    }

    #[test]
    fn fallback_suffixes_stay_valid() {
        for _ in 0..64 {
            let name = format!("user-{}", short_random_suffix());
            assert!(is_valid(&name), "{name} must pass the shape rules");
        }
    }

    #[test]
    fn collision_suffixes_come_from_the_sanitized_stem() {
        let stem = sanitize("User@Example.test");
        assert_eq!(stem, "user");
        assert_eq!(with_suffix(&stem, 2), "user-2");
        assert!(is_valid(&with_suffix(&stem, 99)));
    }

    #[test]
    fn collision_suffixes_never_overflow_the_limit() {
        let stem = "a".repeat(USERNAME_MAX);
        for round in [2, 10, 100] {
            let name = with_suffix(&stem, round);
            assert_eq!(name.chars().count(), USERNAME_MAX, "{name}");
            assert!(is_valid(&name), "{name}");
            assert!(name.ends_with(&format!("-{round}")), "{name}");
        }
    }
}

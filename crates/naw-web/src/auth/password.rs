//! Local passwords: Argon2id hashing, verification, temporary passwords and
//! the rules a new password has to meet.
//!
//! Hashing is deliberately slow and memory hungry, so every call here runs on
//! the blocking pool: a burst of sign-ins must not stall the async workers that
//! serve pages.

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use rand::Rng;

/// Shortest acceptable password, in characters. Length does more than any
/// composition rule, and ten is where guessing stops being a weekend job.
pub const MIN_LEN: usize = 10;
/// Longest accepted password. Argon2 does not care, but an unbounded field is
/// a free way to make the server hash megabytes.
pub const MAX_LEN: usize = 256;

/// Why a new password was refused. Each maps to its own message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    TooShort,
    TooLong,
    SameAsUsername,
    OneCharacter,
    Mismatch,
    Unchanged,
}

impl Problem {
    /// The message key under `account.` in the language packs.
    pub fn key(self) -> &'static str {
        match self {
            Self::TooShort => "password_too_short",
            Self::TooLong => "password_too_long",
            Self::SameAsUsername => "password_is_username",
            Self::OneCharacter => "password_one_character",
            Self::Mismatch => "password_mismatch",
            Self::Unchanged => "password_unchanged",
        }
    }
}

/// Checks a new password against the rules, before anything is hashed.
pub fn check_new(
    password: &str,
    confirm: &str,
    username: &str,
    previous: Option<&str>,
) -> Result<(), Problem> {
    let len = password.chars().count();
    if len < MIN_LEN {
        return Err(Problem::TooShort);
    }
    if len > MAX_LEN {
        return Err(Problem::TooLong);
    }
    if password != confirm {
        return Err(Problem::Mismatch);
    }
    if password.eq_ignore_ascii_case(username) {
        return Err(Problem::SameAsUsername);
    }
    let mut chars = password.chars();
    let first = chars.next().unwrap_or_default();
    if chars.all(|c| c == first) {
        return Err(Problem::OneCharacter);
    }
    if previous == Some(password) {
        return Err(Problem::Unchanged);
    }
    Ok(())
}

/// Hashes a password into a PHC string (`$argon2id$v=19$...`).
pub async fn hash(password: String) -> Result<String, naw_core::error::AppError> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
    })
    .await
    .map_err(|err| {
        tracing::error!(error = %err, "password hashing task failed");
        naw_core::error::AppError::Internal
    })?
    .map_err(|err| {
        tracing::error!(error = %err, "password hashing failed");
        naw_core::error::AppError::Internal
    })
}

/// A hash of nothing in particular, verified against when the account does
/// not exist, so "no such user" and "wrong password" take the same time.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        Argon2::default()
            .hash_password(b"naw timing equalizer")
            .map(|hash| hash.to_string())
            .unwrap_or_default()
    })
}

/// Verifies `password` against a stored PHC string. `None` (no account, or an
/// account without a password) still spends the full verification time and
/// then answers false.
pub async fn verify(password: String, stored: Option<String>) -> bool {
    let known = stored.is_some();
    let result = tokio::task::spawn_blocking(move || {
        let phc = stored.unwrap_or_else(|| dummy_hash().to_string());
        let Ok(parsed) = PasswordHash::new(&phc) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .unwrap_or(false);
    known && result
}

/// Alphabet for temporary passwords: no `0 o 1 l i`, so a password read out
/// over voice chat or copied off a phone screen arrives intact.
const TEMP_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// A temporary password like `k7m2-x9qa-rt4e-hn3w`: sixteen characters from a
/// 31 symbol alphabet, about 79 bits, in groups that are easy to read aloud.
pub fn temporary() -> String {
    let mut rng = rand::rng();
    let mut out = String::with_capacity(19);
    for i in 0..16 {
        if i > 0 && i % 4 == 0 {
            out.push('-');
        }
        let index = rng.random_range(0..TEMP_ALPHABET.len());
        out.push(TEMP_ALPHABET[index] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hash_verifies_its_own_password_only() {
        let hash = hash("correct horse battery".to_string())
            .await
            .expect("hash");
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify("correct horse battery".to_string(), Some(hash.clone())).await);
        assert!(!verify("correct horse batterx".to_string(), Some(hash)).await);
    }

    #[tokio::test]
    async fn a_missing_account_never_verifies() {
        assert!(!verify("naw timing equalizer".to_string(), None).await);
        assert!(!verify("anything".to_string(), Some("not a phc string".to_string())).await);
    }

    #[test]
    fn new_password_rules() {
        let ok = "a long enough one";
        assert_eq!(check_new(ok, ok, "someone", None), Ok(()));
        assert_eq!(
            check_new("short", "short", "u", None),
            Err(Problem::TooShort)
        );
        let long = "x".repeat(MAX_LEN) + "y";
        assert_eq!(check_new(&long, &long, "u", None), Err(Problem::TooLong));
        assert_eq!(
            check_new(ok, "different", "u", None),
            Err(Problem::Mismatch)
        );
        assert_eq!(
            check_new("snackerfan", "snackerfan", "SnackerFan", None),
            Err(Problem::SameAsUsername)
        );
        assert_eq!(
            check_new("aaaaaaaaaaaa", "aaaaaaaaaaaa", "u", None),
            Err(Problem::OneCharacter)
        );
        assert_eq!(check_new(ok, ok, "u", Some(ok)), Err(Problem::Unchanged));
        // Length counts characters, not bytes: ten Cyrillic letters are enough.
        let ru = "пароль-тут";
        assert_eq!(check_new(ru, ru, "u", None), Ok(()));
    }

    #[test]
    fn temporary_passwords_are_readable_and_distinct() {
        let a = temporary();
        let b = temporary();
        assert_ne!(a, b);
        assert_eq!(a.len(), 19);
        assert_eq!(a.matches('-').count(), 3);
        assert!(
            a.chars()
                .filter(|c| *c != '-')
                .all(|c| TEMP_ALPHABET.contains(&(c as u8)))
        );
        assert_eq!(check_new(&a, &a, "someone", None), Ok(()));
    }
}

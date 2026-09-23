//! Local passwords: Argon2id, temporary passwords and the rules for a new one.
//! Hashing runs on the blocking pool so sign-ins never stall page serving.

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use rand::Rng;

/// Shortest acceptable password, in characters.
pub const MIN_LEN: usize = 10;
/// Longest accepted password, so nobody makes the server hash megabytes.
pub const MAX_LEN: usize = 256;

/// Why a new password was refused.
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
    /// The message key under `account.`.
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

/// Checks a new password against the rules before hashing.
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

/// Hashes a password into a PHC string.
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

/// Verified against when there is no account, so timing reveals nothing.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        Argon2::default()
            .hash_password(b"naw timing equalizer")
            .map(|hash| hash.to_string())
            .unwrap_or_default()
    })
}

/// Verifies against a stored PHC string; `None` costs the same and fails.
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

/// No `0 o 1 l i`, so a password read aloud arrives intact.
const TEMP_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// A temporary password like `k7m2-x9qa-rt4e-hn3w`, about 79 bits.
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

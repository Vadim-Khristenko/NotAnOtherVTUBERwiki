//! Sessions and OAuth for the web layer. Provider tokens are never stored,
//! and errors shown to people are generic while the log has the detail.

pub mod http;
pub mod jwks;
pub mod mailer;
pub mod password;
pub mod pkce;
pub mod providers;
pub mod redirect;
pub mod render;
pub mod routes;
pub mod session;
pub mod state_store;
pub mod store;
pub mod throttle;
pub mod types;
pub mod username;

pub use types::AuthError;

use base64::engine::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// PKCE challenge per RFC 7636 Appendix B: base64url(sha256(verifier)).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 32 random bytes, base64url without padding.
pub fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 of a token, as stored.
pub fn token_hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B vector.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn pkce_matches_the_rfc_vector() {
        assert_eq!(pkce_challenge(RFC_VERIFIER), RFC_CHALLENGE);
    }

    #[test]
    fn random_tokens_are_unique_and_unpadded() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(!a.contains('='));
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn token_hash_is_stable_and_binds_to_the_input() {
        let a = token_hash("naw-test-token");
        let b = token_hash("naw-test-token");
        let c = token_hash("naw-test-other");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}

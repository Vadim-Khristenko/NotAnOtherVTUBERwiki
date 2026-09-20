//! Session and OAuth plumbing for the web layer.
//!
//! House rules from the auth guide:
//! provider tokens are never stored, the cache never serves logged-in
//! chrome, and every error shown to the user is generic while the detail
//! lands in the log.
//!
//! The provider round trip is live. What is still idle carries its own
//! narrow `dead_code` allowance at the definition, so that anything newly
//! unused here fails the build instead of hiding behind a module-wide waiver.

pub mod http;
pub mod jwks;
pub mod mailer;
pub mod pkce;
pub mod providers;
pub mod redirect;
pub mod render;
pub mod routes;
pub mod session;
pub mod state_store;
pub mod store;
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

/// 32 random bytes, base64url without padding. Verifiers, states and tokens
/// all come from here.
pub fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Constant-time comparison for token hashing on the read side.
pub fn token_hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B vector.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    /// RFC 7636 Appendix B vector.
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

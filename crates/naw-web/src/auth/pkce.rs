//! PKCE verifier and challenge pair.

use crate::auth::{pkce_challenge, random_token};

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate() -> Pkce {
    let verifier = random_token();
    Pkce {
        challenge: pkce_challenge(&verifier),
        verifier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_its_verifier() {
        let pair = generate();
        assert_eq!(pair.challenge, pkce_challenge(&pair.verifier));
        assert_eq!(pair.verifier.len(), 43);
    }
}

//! JWKS fetching and caching, and id_token verification.
//!
//! The algorithm comes from the JWK, never from the token header, which is
//! attacker controlled; the header only picks a `kid`. Key sets are cached in
//! Valkey per issuer, and an unknown `kid` triggers exactly one refetch.

use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::de::DeserializeOwned;

use super::http::HttpFetch;
use super::types::AuthError;

/// Providers rotate keys slowly.
const CACHE_TTL_SECONDS: u64 = 3600;

fn cache_key(issuer: &str) -> String {
    format!("auth:jwks:{}", hex::encode(super::token_hash(issuer)))
}

/// The cached key set for `issuer`, if usable.
async fn cached(cache: &deadpool_redis::Pool, issuer: &str) -> Option<JwkSet> {
    let mut conn = cache.get().await.ok()?;
    let raw: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg(cache_key(issuer))
        .query_async(&mut conn)
        .await
        .ok()?;
    serde_json::from_str(&raw?).ok()
}

async fn store(cache: &deadpool_redis::Pool, issuer: &str, raw: &str) {
    let Ok(mut conn) = cache.get().await else {
        return;
    };
    let _: Result<(), _> = deadpool_redis::redis::cmd("SET")
        .arg(cache_key(issuer))
        .arg(raw)
        .arg("EX")
        .arg(CACHE_TTL_SECONDS)
        .query_async::<()>(&mut conn)
        .await;
}

async fn fetch(http: &dyn HttpFetch, jwks_uri: &str) -> Result<(JwkSet, String), AuthError> {
    let response = http.get(jwks_uri, None, &[]).await?;
    if !response.is_success() {
        return Err(AuthError::Upstream(format!(
            "jwks fetch returned {}",
            response.status
        )));
    }
    let raw = String::from_utf8(response.body.clone())
        .map_err(|_| AuthError::Upstream("jwks was not utf8".to_string()))?;
    let set: JwkSet = response.json()?;
    if set.keys.is_empty() {
        return Err(AuthError::Upstream("jwks carried no keys".to_string()));
    }
    Ok((set, raw))
}

/// The algorithm this key may verify, from the key itself. `None` for keys
/// `jsonwebtoken` cannot verify (ES256K) or that make no sense here.
pub fn algorithm_of(jwk: &Jwk) -> Option<Algorithm> {
    use jsonwebtoken::jwk::{EllipticCurve, KeyAlgorithm};

    if let Some(declared) = jwk.common.key_algorithm {
        return match declared {
            KeyAlgorithm::RS256 => Some(Algorithm::RS256),
            KeyAlgorithm::RS384 => Some(Algorithm::RS384),
            KeyAlgorithm::RS512 => Some(Algorithm::RS512),
            KeyAlgorithm::PS256 => Some(Algorithm::PS256),
            KeyAlgorithm::PS384 => Some(Algorithm::PS384),
            KeyAlgorithm::PS512 => Some(Algorithm::PS512),
            KeyAlgorithm::ES256 => Some(Algorithm::ES256),
            KeyAlgorithm::ES384 => Some(Algorithm::ES384),
            KeyAlgorithm::EdDSA => Some(Algorithm::EdDSA),
            _ => None,
        };
    }
    // No `alg` on the key: infer only where the curve is unambiguous.
    match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Some(Algorithm::RS256),
        AlgorithmParameters::EllipticCurve(ec) => match ec.curve {
            EllipticCurve::P256 => Some(Algorithm::ES256),
            EllipticCurve::P384 => Some(Algorithm::ES384),
            _ => None,
        },
        AlgorithmParameters::OctetKeyPair(_) => Some(Algorithm::EdDSA),
        // Symmetric keys in a provider JWKS are the alg-confusion setup.
        AlgorithmParameters::OctetKey(_) => None,
        // `AlgorithmParameters` is non_exhaustive: unknown means refuse.
        _ => None,
    }
}

/// What an id_token must satisfy beyond its signature.
pub struct Expect<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    pub jwks_uri: &'a str,
}

/// Verifies `token` and returns its claims. Without `cache` the key set is
/// fetched every time.
pub async fn verify<T: DeserializeOwned>(
    token: &str,
    expect: &Expect<'_>,
    http: &dyn HttpFetch,
    cache: Option<&deadpool_redis::Pool>,
) -> Result<T, AuthError> {
    let header =
        decode_header(token).map_err(|_| AuthError::BadRequest("id_token header is malformed"))?;
    let Some(kid) = header.kid else {
        return Err(AuthError::BadRequest("id_token carries no kid"));
    };

    let mut set = match cache {
        Some(pool) => cached(pool, expect.issuer).await,
        None => None,
    };
    // One refetch for an unknown kid (rotation); a further miss is a bad token.
    if set.as_ref().is_none_or(|keys| keys.find(&kid).is_none()) {
        let (fresh, raw) = fetch(http, expect.jwks_uri).await?;
        if let Some(pool) = cache {
            store(pool, expect.issuer, &raw).await;
        }
        set = Some(fresh);
    }

    let set = set.ok_or_else(|| AuthError::Upstream("jwks unavailable".to_string()))?;
    let jwk = set
        .find(&kid)
        .ok_or(AuthError::BadRequest("id_token kid is not published"))?;

    let algorithm = algorithm_of(jwk).ok_or_else(|| {
        AuthError::Upstream(format!(
            "provider signed with an algorithm we cannot verify: {:?}",
            jwk.common.key_algorithm
        ))
    })?;
    let key = DecodingKey::from_jwk(jwk)
        .map_err(|err| AuthError::Upstream(format!("jwk is unusable: {err}")))?;

    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[expect.issuer]);
    validation.set_audience(&[expect.audience]);
    validation.required_spec_claims = ["exp", "iss", "aud"]
        .iter()
        .map(|c| c.to_string())
        .collect();

    decode::<T>(token, &key, &validation)
        .map(|data| data.claims)
        .map_err(|err| {
            tracing::warn!(error = %err, "id_token rejected");
            AuthError::BadRequest("id_token failed validation")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Telegram's published key set, trimmed.
    const TELEGRAM_JWKS: &str = r#"{"keys":[
      {"alg":"RS256","e":"AQAB","kty":"RSA","kid":"oidc-1",
       "n":"5RneLtsKvVcxdv6gu6gxEQu30Cru5NiMQnY6SNr9ZyZFZ4ya-pfHNuaZXJ6QPG0JSFwoxeOkEO2-eZN_REVPm448PvjjsR1eQdZ5QpEkNxnItFcmxkHH91v5cgf52_EI9BGO-MT6f1vaBSg3uWHFlDxI7J2AYxNvd1_Nf3TkgrrR7gyJFTmEIai5RefGnA0KGNYDlRIGUzrz2F05n6gTaHFT_iHL5UHatTZA4GCiUSjIOuwqu5pE5uZge20TFv3cxXMQaFw_xv1pgQt_Rq8eoCN7TS0RQ0zjWKiad-W286BcFectXsUm03p5Nq_kY4mf_7rqwX_B8yy_bBreyKn7RQ"},
      {"alg":"ES256","kty":"EC","crv":"P-256","kid":"oidc-es256-1","use":"sig",
       "x":"ahVYrohhX6YA7w0P2gUNSwMFbaabCgBZFkeq9bWdmwU",
       "y":"Ea8nKJ34VQMA7zv8aYDfzcBhXEjnWQ9C06jVke_eUV0"},
      {"alg":"EdDSA","crv":"Ed25519","kid":"oidc-eddsa-1","kty":"OKP","use":"sig",
       "x":"i6BEafXMEe4osXgUTffpKAm6Cn6F2bhqPZoclunTAV4"},
      {"alg":"ES256K","kty":"EC","crv":"secp256k1","kid":"oidc-es256k-1","use":"sig",
       "x":"vsk5i5YJu8H_VPL7DWTgVGXBPrqgkyNmYvfgOrVut38",
       "y":"Mrg56tBhVeorHPXK1LbTX2jP7rEqOHIatM96HFzVMIU"}]}"#;

    fn telegram_set() -> JwkSet {
        serde_json::from_str(TELEGRAM_JWKS).expect("telegram publishes this")
    }

    #[test]
    fn the_real_telegram_key_set_parses() {
        let set = telegram_set();
        assert_eq!(set.keys.len(), 4);
        for kid in ["oidc-1", "oidc-es256-1", "oidc-eddsa-1", "oidc-es256k-1"] {
            assert!(set.find(kid).is_some(), "{kid} must be found by kid");
        }
    }

    #[test]
    fn each_published_key_maps_to_the_algorithm_it_declares() {
        let set = telegram_set();
        assert_eq!(
            algorithm_of(set.find("oidc-1").unwrap()),
            Some(Algorithm::RS256)
        );
        assert_eq!(
            algorithm_of(set.find("oidc-es256-1").unwrap()),
            Some(Algorithm::ES256)
        );
        assert_eq!(
            algorithm_of(set.find("oidc-eddsa-1").unwrap()),
            Some(Algorithm::EdDSA)
        );
    }

    #[test]
    fn es256k_is_refused_instead_of_being_coerced() {
        let set = telegram_set();
        assert_eq!(algorithm_of(set.find("oidc-es256k-1").unwrap()), None);
    }

    #[test]
    fn a_symmetric_key_is_never_accepted_from_a_jwks() {
        // Verifying RS256 as HS256 with the public key as the HMAC secret.
        let jwk: Jwk = serde_json::from_str(
            r#"{"kty":"oct","kid":"sneaky","k":"c2VjcmV0LWtleS1tYXRlcmlhbA"}"#,
        )
        .expect("parses");
        assert_eq!(algorithm_of(&jwk), None);
    }

    #[test]
    fn a_key_without_alg_is_inferred_only_when_unambiguous() {
        let rsa: Jwk = serde_json::from_str(
            r#"{"kty":"RSA","kid":"r","e":"AQAB","n":"5RneLtsKvVcxdv6gu6gxEQu30Cru5NiMQnY6SNr9ZyZFZ4ya-pfHNuaZXJ6QPG0JSFwoxeOkEO2-eZN_REVPm448PvjjsR1eQdZ5QpEkNxnItFcmxkHH91v5cgf52_EI9BGO-MT6f1vaBSg3uWHFlDxI7J2AYxNvd1_Nf3TkgrrR7gyJFTmEIai5RefGnA0KGNYDlRIGUzrz2F05n6gTaHFT_iHL5UHatTZA4GCiUSjIOuwqu5pE5uZge20TFv3cxXMQaFw_xv1pgQt_Rq8eoCN7TS0RQ0zjWKiad-W286BcFectXsUm03p5Nq_kY4mf_7rqwX_B8yy_bBreyKn7RQ"}"#,
        )
        .expect("parses");
        assert_eq!(algorithm_of(&rsa), Some(Algorithm::RS256));

        let k256: Jwk = serde_json::from_str(
            r#"{"kty":"EC","kid":"k","crv":"secp256k1","x":"vsk5i5YJu8H_VPL7DWTgVGXBPrqgkyNmYvfgOrVut38","y":"Mrg56tBhVeorHPXK1LbTX2jP7rEqOHIatM96HFzVMIU"}"#,
        )
        .expect("parses");
        assert_eq!(algorithm_of(&k256), None, "no alg and no safe inference");
    }

    #[test]
    fn cache_keys_are_hashed_and_namespaced_per_issuer() {
        let telegram = cache_key("https://oauth.telegram.org");
        assert!(telegram.starts_with("auth:jwks:"));
        assert!(
            !telegram.contains("telegram"),
            "issuer stays out of the key"
        );
        assert_ne!(telegram, cache_key("https://accounts.google.com"));
    }

    #[tokio::test]
    async fn a_token_without_a_kid_is_refused_before_any_fetch() {
        use crate::auth::http::test_double::Scripted;
        let token = "eyJhbGciOiJub25lIn0.eyJpc3MiOiJodHRwczovL29hdXRoLnRlbGVncmFtLm9yZyJ9.";
        let http = Scripted::new();
        let err = verify::<serde_json::Value>(
            token,
            &Expect {
                issuer: "https://oauth.telegram.org",
                audience: "1234",
                jwks_uri: "https://oauth.telegram.org/.well-known/jwks.json",
            },
            &http,
            None,
        )
        .await
        .expect_err("no kid, no verification");
        assert!(matches!(err, AuthError::BadRequest(_)));
        assert!(
            http.calls().is_empty(),
            "a malformed token must not cause upstream traffic"
        );
    }

    #[tokio::test]
    async fn an_unknown_kid_refetches_once_and_then_gives_up() {
        use crate::auth::http::test_double::Scripted;
        let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6InJvdGF0ZWQtYXdheSJ9.eyJpc3MiOiJ4In0.sig";
        let http = Scripted::new().on("jwks.json", 200, TELEGRAM_JWKS);
        let err = verify::<serde_json::Value>(
            token,
            &Expect {
                issuer: "https://oauth.telegram.org",
                audience: "1234",
                jwks_uri: "https://oauth.telegram.org/.well-known/jwks.json",
            },
            &http,
            None,
        )
        .await
        .expect_err("kid is not published");
        assert!(matches!(err, AuthError::BadRequest(_)));
        assert_eq!(
            http.calls().len(),
            1,
            "exactly one refetch, not a retry loop"
        );
    }

    #[tokio::test]
    async fn an_empty_key_set_is_an_upstream_problem() {
        use crate::auth::http::test_double::Scripted;
        let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6Im9pZGMtMSJ9.eyJpc3MiOiJ4In0.sig";
        let http = Scripted::new().on("jwks.json", 200, r#"{"keys":[]}"#);
        let err = verify::<serde_json::Value>(
            token,
            &Expect {
                issuer: "https://oauth.telegram.org",
                audience: "1234",
                jwks_uri: "https://oauth.telegram.org/.well-known/jwks.json",
            },
            &http,
            None,
        )
        .await
        .expect_err("no keys");
        assert!(matches!(err, AuthError::Upstream(_)));
    }

    #[tokio::test]
    async fn a_forged_signature_on_a_real_kid_does_not_verify() {
        use crate::auth::http::test_double::Scripted;
        let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6Im9pZGMtMSJ9.eyJpc3MiOiJodHRwczovL29hdXRoLnRlbGVncmFtLm9yZyIsImF1ZCI6IjEyMzQiLCJleHAiOjk5OTk5OTk5OTksInN1YiI6IjEifQ.not-a-real-signature";
        let http = Scripted::new().on("jwks.json", 200, TELEGRAM_JWKS);
        let err = verify::<serde_json::Value>(
            token,
            &Expect {
                issuer: "https://oauth.telegram.org",
                audience: "1234",
                jwks_uri: "https://oauth.telegram.org/.well-known/jwks.json",
            },
            &http,
            None,
        )
        .await
        .expect_err("the signature is not valid");
        assert!(matches!(err, AuthError::BadRequest(_)));
    }
}

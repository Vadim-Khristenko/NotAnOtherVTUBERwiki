//! Login flow state, stored in Valkey, single use.

use serde::{Deserialize, Serialize};

use crate::auth::{random_token, token_hash};

/// Ten minutes to finish the provider round trip.
const TTL_SECONDS: u64 = 600;

/// Plain sign-in, or attaching an identity to the current account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Login,
    Link,
}

#[derive(Debug)]
pub struct FlowState {
    pub provider: String,
    pub mode: Flow,
    pub verifier: String,
    pub nonce: String,
    /// Guarded same-site path from `?next=`.
    pub next: String,
    /// The account for a link flow.
    pub user_id: Option<uuid::Uuid>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    provider: String,
    mode: Flow,
    verifier: String,
    nonce: String,
    next: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<uuid::Uuid>,
}

impl Serialize for Flow {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Flow::Login => serializer.serialize_str("login"),
            Flow::Link => serializer.serialize_str("link"),
        }
    }
}

impl<'de> Deserialize<'de> for Flow {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "link" => Ok(Flow::Link),
            _ => Ok(Flow::Login),
        }
    }
}

fn key(state: &str) -> String {
    format!("auth:state:{}", hex::encode(token_hash(state)))
}

/// Stores the flow in Valkey and returns its state token.
pub async fn begin(
    valkey: &deadpool_redis::Pool,
    state_in: FlowState,
) -> Result<String, crate::auth::AuthError> {
    let state = random_token();
    let stored = Stored {
        provider: state_in.provider,
        mode: state_in.mode,
        verifier: state_in.verifier,
        nonce: state_in.nonce,
        next: state_in.next,
        user_id: state_in.user_id,
    };
    let payload = serde_json::to_string(&stored)
        .map_err(|_| crate::auth::AuthError::Upstream("state encode failed".to_string()))?;
    let mut conn = valkey
        .get()
        .await
        .map_err(|_| crate::auth::AuthError::Upstream("cache unavailable".to_string()))?;
    let ttl = std::time::Duration::from_secs(TTL_SECONDS);
    deadpool_redis::redis::cmd("SET")
        .arg(key(&state))
        .arg(payload)
        .arg("EX")
        .arg(ttl.as_secs())
        .query_async::<()>(&mut conn)
        .await
        .map_err(|_| crate::auth::AuthError::Upstream("cache write failed".to_string()))?;
    Ok(state)
}

/// Takes the state (GETDEL, single use) when it matches the provider.
pub async fn take(
    valkey: &deadpool_redis::Pool,
    state: &str,
    provider: &str,
) -> Result<FlowState, crate::auth::AuthError> {
    let mut conn = valkey
        .get()
        .await
        .map_err(|_| crate::auth::AuthError::Upstream("cache unavailable".to_string()))?;
    let raw: Option<String> = deadpool_redis::redis::cmd("GETDEL")
        .arg(key(state))
        .query_async(&mut conn)
        .await
        .map_err(|_| crate::auth::AuthError::Upstream("cache read failed".to_string()))?;
    let Some(raw) = raw else {
        return Err(crate::auth::AuthError::StateExpired);
    };
    let stored: Stored =
        serde_json::from_str(&raw).map_err(|_| crate::auth::AuthError::StateExpired)?;
    if stored.provider != provider {
        return Err(crate::auth::AuthError::StateExpired);
    }
    Ok(FlowState {
        provider: stored.provider,
        mode: stored.mode,
        verifier: stored.verifier,
        nonce: stored.nonce,
        next: stored.next,
        user_id: stored.user_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn keys_are_hashed_and_namespaced() {
        let k = key("some-state-value");
        assert!(k.starts_with("auth:state:"));
        let expected = hex::encode(Sha256::digest(b"some-state-value"));
        assert_eq!(k, format!("auth:state:{expected}"));
        assert!(
            !k.contains("some-state-value"),
            "the raw state must never reach the cache key"
        );
        assert_ne!(k, key("some-state-valuf"));
    }
    #[test]
    fn flow_json_round_trips_without_leaking_options() {
        let stored = Stored {
            provider: "github".to_string(),
            mode: Flow::Link,
            verifier: "v".to_string(),
            nonce: "n".to_string(),
            next: "/".to_string(),
            user_id: None,
        };
        let raw = serde_json::to_string(&stored).expect("serializes");
        assert!(raw.contains("\"mode\":\"link\""));
        assert!(
            !raw.contains("user_id"),
            "None must stay out of the payload"
        );
        let back: Stored = serde_json::from_str(&raw).expect("deserializes");
        assert_eq!(back.mode, Flow::Link);
        assert!(back.user_id.is_none());
    }
}

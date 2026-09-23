//! Slows down password guessing with two Valkey counters per 15 minutes:
//! per account name and per client address. A correct password clears the
//! name's counter. With Valkey down the throttle lets requests through.

use std::net::IpAddr;

use deadpool_redis::Pool;

/// Misses per name before sign-in pauses for it.
pub const PER_ACCOUNT: i64 = 10;
/// Misses per address, across all names.
pub const PER_ADDRESS: i64 = 50;
/// Window length from its first miss.
pub const WINDOW_SECS: i64 = 15 * 60;

fn account_key(username: &str) -> String {
    // Bounded, so a huge username cannot become a huge key.
    let name: String = username.trim().to_lowercase().chars().take(64).collect();
    format!("naw:login:fail:user:{name}")
}

fn address_key(ip: IpAddr) -> String {
    format!("naw:login:fail:ip:{ip}")
}

/// Whether this name or this address used up its misses.
pub async fn is_blocked(valkey: &Pool, username: &str, ip: IpAddr) -> bool {
    let Ok(mut conn) = valkey.get().await else {
        tracing::error!("valkey unavailable, the sign-in throttle is not enforced");
        return false;
    };
    let counts: Result<(Option<i64>, Option<i64>), _> = deadpool_redis::redis::pipe()
        .cmd("GET")
        .arg(account_key(username))
        .cmd("GET")
        .arg(address_key(ip))
        .query_async(&mut conn)
        .await;
    match counts {
        Ok((account, address)) => {
            account.unwrap_or(0) >= PER_ACCOUNT || address.unwrap_or(0) >= PER_ADDRESS
        }
        Err(err) => {
            tracing::error!(error = %err, "sign-in throttle read failed");
            false
        }
    }
}

/// Counts one wrong password against the name and the address.
pub async fn record_miss(valkey: &Pool, username: &str, ip: IpAddr) {
    let Ok(mut conn) = valkey.get().await else {
        return;
    };
    // EXPIRE NX: later misses do not extend the window.
    let result: Result<(), _> = deadpool_redis::redis::pipe()
        .cmd("INCR")
        .arg(account_key(username))
        .ignore()
        .cmd("EXPIRE")
        .arg(account_key(username))
        .arg(WINDOW_SECS)
        .arg("NX")
        .ignore()
        .cmd("INCR")
        .arg(address_key(ip))
        .ignore()
        .cmd("EXPIRE")
        .arg(address_key(ip))
        .arg(WINDOW_SECS)
        .arg("NX")
        .ignore()
        .query_async(&mut conn)
        .await;
    if let Err(err) = result {
        tracing::error!(error = %err, "sign-in throttle write failed");
    }
}

/// A correct password clears the name's count; the address keeps its own.
pub async fn clear_account(valkey: &Pool, username: &str) {
    if let Ok(mut conn) = valkey.get().await {
        let _: Result<(), _> = deadpool_redis::redis::cmd("DEL")
            .arg(account_key(username))
            .query_async(&mut conn)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_normalized_and_bounded() {
        assert_eq!(account_key("  VAI "), "naw:login:fail:user:vai");
        let long = "a".repeat(10_000);
        assert!(account_key(&long).len() < 100);
        assert_eq!(
            address_key("198.51.100.7".parse().unwrap()),
            "naw:login:fail:ip:198.51.100.7"
        );
    }
}

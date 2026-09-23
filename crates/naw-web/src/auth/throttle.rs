//! Slows down password guessing.
//!
//! Two counters in Valkey, both reset fifteen minutes after the first miss:
//! one per account name, so a slow spray across many addresses cannot walk
//! through one password list, and one per client address, so one address
//! cannot walk through many accounts. A correct password clears the account
//! counter.
//!
//! When Valkey is down the throttle lets requests through and says so in the
//! log. Failing closed would lock every account out because of a cache.

use std::net::IpAddr;

use deadpool_redis::Pool;

/// Misses per account name before sign-in pauses for that name.
pub const PER_ACCOUNT: i64 = 10;
/// Misses per address, across all names.
pub const PER_ADDRESS: i64 = 50;
/// How long a window lasts, from its first miss.
pub const WINDOW_SECS: i64 = 15 * 60;

fn account_key(username: &str) -> String {
    // Bounded, so a megabyte "username" cannot become a megabyte key.
    let name: String = username.trim().to_lowercase().chars().take(64).collect();
    format!("naw:login:fail:user:{name}")
}

fn address_key(ip: IpAddr) -> String {
    format!("naw:login:fail:ip:{ip}")
}

/// True when this name or this address used up its misses for now.
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
    // INCR then EXPIRE NX: the window starts at the first miss and later misses
    // do not extend it, so a locked name unlocks on schedule.
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

/// A correct password clears the name's counter. The address keeps its count:
/// one success in a spray of misses is not a reason to forget the spray.
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

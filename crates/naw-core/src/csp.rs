//! The script nonce of the response being built.
//!
//! The web layer draws a fresh nonce per request, allows it in the
//! Content-Security-Policy header and runs the handler inside [`scope`].
//! Templates print it with `csp_nonce()` on every inline `<script>`, so the
//! engine's own scripts run and a script that reached a page any other way,
//! through some future escaping bug, does not.

use std::future::Future;

tokio::task_local! {
    static NONCE: String;
}

/// Runs `future` with `nonce` as the current one.
pub async fn scope<F: Future>(nonce: String, future: F) -> F::Output {
    NONCE.scope(nonce, future).await
}

/// The nonce of the current request. Empty outside one, where no policy
/// header goes out either.
pub fn current() -> String {
    NONCE.try_with(String::clone).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_nonce_is_there_inside_the_scope_only() {
        assert_eq!(current(), "");
        let inside = scope("abc".to_string(), async { current() }).await;
        assert_eq!(inside, "abc");
        assert_eq!(current(), "");
    }
}

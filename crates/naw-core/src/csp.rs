//! The script nonce of the response being rendered.
//!
//! The web layer draws a nonce per request, allows it in the CSP header and
//! runs the handler inside [`scope`]; templates read it with `csp_nonce()`.

use std::future::Future;

tokio::task_local! {
    static NONCE: String;
}

/// Runs `future` with `nonce` as the current one.
pub async fn scope<F: Future>(nonce: String, future: F) -> F::Output {
    NONCE.scope(nonce, future).await
}

/// The current request's nonce, or empty outside a request.
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

//! Open-redirect guard for `?next=` and other user-controlled targets.

/// Percent-encodes a query parameter value, keeping only the RFC 3986
/// unreserved set.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `raw` when it is a same-site absolute path (one leading `/`, no scheme,
/// authority or backslash), `/` otherwise.
pub fn safe_next(raw: Option<&str>) -> String {
    match raw {
        Some(value) if is_safe_path(value) => value.to_string(),
        _ => "/".to_string(),
    }
}

fn is_safe_path(value: &str) -> bool {
    if !value.starts_with('/') || value.starts_with("//") || value.contains('\\') {
        return false;
    }
    !value.chars().any(|c| c.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_pass() {
        assert_eq!(safe_next(Some("/home")), "/home");
        assert_eq!(safe_next(Some("/settings/account")), "/settings/account");
        assert_eq!(safe_next(Some("/")), "/");
    }

    #[test]
    fn off_site_and_tricks_fall_back() {
        assert_eq!(safe_next(Some("//evil.test")), "/");
        assert_eq!(safe_next(Some("https://evil.test")), "/");
        assert_eq!(safe_next(Some("\\\\evil.test")), "/");
        assert_eq!(safe_next(Some("/a\\b")), "/");
        assert_eq!(safe_next(Some("javascript:alert(1)")), "/");
        assert_eq!(safe_next(Some("/ho\nme")), "/");
        assert_eq!(safe_next(Some("")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn relative_junk_falls_back() {
        assert_eq!(safe_next(Some("home")), "/");
        assert_eq!(safe_next(Some("../home")), "/");
    }
}

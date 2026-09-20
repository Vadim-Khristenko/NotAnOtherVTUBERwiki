#![allow(non_snake_case)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn base_address() -> String {
    std::env::var("NAW_TEST_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:4242".to_string())
}

fn request(method: &str, path: &str, body: Option<&[u8]>) -> Result<HttpResponse, String> {
    let base = base_address();
    let address = base
        .strip_prefix("http://")
        .ok_or_else(|| "NAW_TEST_BASE_URL must use http://".to_string())?
        .trim_end_matches('/');
    let socket = address
        .to_socket_addrs()
        .map_err(|err| format!("resolve {address}: {err}"))?
        .next()
        .ok_or_else(|| format!("no address for {address}"))?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_secs(3))
        .map_err(|err| format!("connect {address}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| format!("set read timeout: {err}"))?;

    let payload = body.unwrap_or_default();
    let content_type = if body.is_some() {
        "Content-Type: application/x-www-form-urlencoded\r\n"
    } else {
        ""
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{content_type}Content-Length: {}\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(payload))
        .map_err(|err| format!("write {method} {path}: {err}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|err| format!("read {method} {path}: {err}"))?;
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Result<HttpResponse, String> {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "HTTP response has no header separator".to_string())?;
    let (head, body) = raw.split_at(separator + 4);
    let head = std::str::from_utf8(&head[..head.len() - 4])
        .map_err(|err| format!("HTTP headers are not UTF-8: {err}"))?;
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "HTTP response has no status".to_string())?
        .parse::<u16>()
        .map_err(|err| format!("invalid HTTP status: {err}"))?;
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    Ok(HttpResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

fn form_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn form(fields: &[(&str, &str)]) -> Vec<u8> {
    fields
        .iter()
        .map(|(name, value)| format!("{}={}", form_encode(name), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

fn assert_not_500(method: &str, path: &str, response: &HttpResponse) {
    assert_ne!(
        response.status, 500,
        "{method} {path} returned an unexpected HTTP 500"
    );
}

fn assert_asset(path: &str, expected_type: &str, signature: &[u8]) {
    let response = request("GET", path, None).unwrap_or_else(|err| panic!("GET {path}: {err}"));
    assert_not_500("GET", path, &response);
    assert_eq!(response.status, 200, "GET {path} did not serve the asset");
    assert_eq!(
        response.headers.get("content-type").map(String::as_str),
        Some(expected_type),
        "GET {path} served the wrong content type"
    );
    assert!(
        response.body.starts_with(signature),
        "GET {path} did not serve the expected binary signature"
    );
    assert!(!response.body.is_empty(), "GET {path} served an empty body");
}

#[test]
fn WeCantGive500() {
    if std::env::var("NAW_RUN_LIVE_TESTS").as_deref() != Ok("1") {
        eprintln!("WeCantGive500 skipped, set NAW_RUN_LIVE_TESTS=1 for live HTTP checks");
        return;
    }

    let get_paths = [
        "/health",
        "/ready",
        "/",
        "/home",
        "/showcase",
        "/does-not-exist",
        "/%3Cscript%3E",
        "/%00",
        "/%FF",
        "/%2e%2e/home",
        "/home?jump_to=inline-styles",
        "/home?jump_to=%3Cscript%3E",
        "/home?jump_to=a%2Fb",
        "/home?jump_to=%00",
        "/home?jump_to=%FF",
        "/favicon.ico",
        "/favicon-96x96.png",
        "/apple-touch-icon.png",
        "/site.webmanifest",
        "/web-app-manifest-192x192.png",
        "/web-app-manifest-512x512.png",
    ];
    for path in get_paths {
        let response = request("GET", path, None).unwrap_or_else(|err| panic!("GET {path}: {err}"));
        assert_not_500("GET", path, &response);
    }

    let post_cases = [
        (
            "/preview",
            form(&[("title", "Preview"), ("body_md", "# hello\n\nText")]),
        ),
        (
            "/preview?fragment=1",
            form(&[("title", "Preview"), ("body_md", "<div>safe input</div>")]),
        ),
        (
            "/preview?fragment=2",
            form(&[("title", ""), ("body_md", "||hidden|| ==red|mark==")]),
        ),
        ("/preview", b"%".to_vec()),
        (
            "/new",
            form(&[("slug", "../home"), ("title", "bad"), ("body_md", "x")]),
        ),
        (
            "/new",
            form(&[("slug", "XX"), ("title", "bad"), ("body_md", "x")]),
        ),
        (
            "/new",
            form(&[("slug", ""), ("title", ""), ("body_md", "")]),
        ),
        (
            "/missing-page/edit",
            form(&[("title", "bad"), ("body_md", "x")]),
        ),
        (
            "/%3Cscript%3E/edit",
            form(&[("title", "bad"), ("body_md", "x")]),
        ),
        ("/health", Vec::new()),
        ("/favicon.ico", Vec::new()),
    ];
    for (path, body) in post_cases {
        let response =
            request("POST", path, Some(&body)).unwrap_or_else(|err| panic!("POST {path}: {err}"));
        assert_not_500("POST", path, &response);
    }

    let oversized_title = "x".repeat(201);
    let response = request(
        "POST",
        "/preview",
        Some(&form(&[("title", &oversized_title), ("body_md", "x")])),
    )
    .expect("oversized title request should complete");
    assert_not_500("POST", "/preview", &response);

    let oversized_body = "x".repeat(500_001);
    let response = request(
        "POST",
        "/preview",
        Some(&form(&[("title", "large"), ("body_md", &oversized_body)])),
    )
    .expect("oversized body request should complete");
    assert_not_500("POST", "/preview", &response);

    for method in ["PUT", "PATCH", "DELETE", "OPTIONS"] {
        let response = request(method, "/favicon.ico", None)
            .unwrap_or_else(|err| panic!("{method} /favicon.ico: {err}"));
        assert_not_500(method, "/favicon.ico", &response);
    }

    if std::env::var("NAW_TEST_REQUIRE_BRAND_ASSETS").as_deref() == Ok("1") {
        assert_asset("/favicon.ico", "image/x-icon", &[0, 0, 1, 0]);
        assert_asset("/favicon-96x96.png", "image/png", b"\x89PNG\r\n\x1a\n");
        assert_asset("/apple-touch-icon.png", "image/png", b"\x89PNG\r\n\x1a\n");
        assert_asset(
            "/web-app-manifest-192x192.png",
            "image/png",
            b"\x89PNG\r\n\x1a\n",
        );
        assert_asset(
            "/web-app-manifest-512x512.png",
            "image/png",
            b"\x89PNG\r\n\x1a\n",
        );
        let manifest = request("GET", "/site.webmanifest", None).expect("manifest request");
        let text = String::from_utf8(manifest.body).expect("manifest must be UTF-8 JSON");
        assert!(
            text.contains("FilianWIKI"),
            "manifest lost the FilianWIKI brand"
        );
    }
}

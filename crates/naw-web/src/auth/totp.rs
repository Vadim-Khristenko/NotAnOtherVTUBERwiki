//! Time-based one-time codes (TOTP, RFC 6238) as authenticator apps make
//! them: an HMAC-SHA1 of the 30-second step, cut to six digits. And the
//! one-time recovery codes for a lost phone.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit as _, XChaCha20Poly1305, XNonce};
use hmac::{Hmac, KeyInit, Mac};
use rand::{Rng, RngCore};
use sha1::Sha1;
use uuid::Uuid;

/// Seconds per step; every common app uses 30.
pub const STEP_SECS: i64 = 30;

/// Steps either side of now a code still counts for, for a phone clock that
/// is a little off.
const DRIFT: i64 = 1;

/// Bytes of key: 160 bits, as RFC 4226 recommends.
const SECRET_LEN: usize = 20;

/// Recovery codes issued at once.
pub const RECOVERY_CODES: usize = 10;

/// A fresh random key.
pub fn new_secret() -> Vec<u8> {
    let mut bytes = vec![0u8; SECRET_LEN];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// The six-digit code for one step.
pub(crate) fn code_at(secret: &[u8], step: i64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let bin = (u32::from(digest[offset]) & 0x7f) << 24
        | u32::from(digest[offset + 1]) << 16
        | u32::from(digest[offset + 2]) << 8
        | u32::from(digest[offset + 3]);
    bin % 1_000_000
}

/// The step `code` was made for, if it is right for now give or take
/// [`DRIFT`] and newer than `last_step`, the one taken before. Spaces in the
/// code are ignored, as apps show it in two halves.
pub fn verify(secret: &[u8], code: &str, unix_now: i64, last_step: Option<i64>) -> Option<i64> {
    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let wanted: u32 = digits.parse().ok()?;
    let now = unix_now.div_euclid(STEP_SECS);
    (now - DRIFT..=now + DRIFT)
        .filter(|step| last_step.is_none_or(|last| *step > last))
        .find(|step| code_at(secret, *step) == wanted)
}

/// RFC 4648 base32 without padding, the form apps take a key in.
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The `otpauth://` address an app reads from the QR code.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let issuer = super::redirect::encode_component(issuer);
    let account = super::redirect::encode_component(account);
    format!(
        "otpauth://totp/{issuer}:{account}?secret={}&issuer={issuer}&algorithm=SHA1&digits=6&period={STEP_SECS}",
        base32(secret)
    )
}

/// The QR code of `text` as an inline SVG, without the XML declaration a
/// standalone file would have.
pub fn qr_svg(text: &str) -> Option<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let svg = code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build();
    let start = svg.find("<svg")?;
    Some(svg[start..].to_string())
}

/// Bytes of the random nonce in front of a sealed key.
const NONCE_LEN: usize = 24;

/// The cipher key from the server secret (`auth.secret_key`), or `None` when
/// it is too short to trust. Hashed under a label, so the same secret could
/// key something else without the two meeting.
pub fn key_from(secret: &str) -> Option<[u8; 32]> {
    let secret = secret.trim();
    (secret.len() >= 32).then(|| super::token_hash(&format!("naw two-step key v1\0{secret}")))
}

/// Encrypts an account's key with XChaCha20-Poly1305: a random nonce, then
/// the ciphertext. The account id is bound in, so a row copied onto another
/// account does not open.
pub fn seal(key: &[u8; 32], user_id: Uuid, secret: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce);
    let sealed = XChaCha20Poly1305::new(key.into())
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: secret,
                aad: user_id.as_bytes(),
            },
        )
        .expect("encrypting a short key in memory does not fail");
    [nonce.as_slice(), &sealed].concat()
}

/// The account's key from [`seal`]'s bytes, or `None` when they were sealed
/// under another server secret, for another account, or changed.
pub fn open(key: &[u8; 32], user_id: Uuid, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() <= NONCE_LEN {
        return None;
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: body,
                aad: user_id.as_bytes(),
            },
        )
        .ok()
}

/// No `0 o 1 l i`, so a code copied by hand arrives intact.
const RECOVERY_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// A recovery code like `k7m2x-9qart`, about 50 bits.
pub fn recovery_code() -> String {
    let mut rng = rand::rng();
    let mut out = String::with_capacity(11);
    for i in 0..10 {
        if i == 5 {
            out.push('-');
        }
        out.push(RECOVERY_ALPHABET[rng.random_range(0..RECOVERY_ALPHABET.len())] as char);
    }
    out
}

/// The stored digest of a recovery code as typed: case, spaces and dashes
/// do not matter.
pub fn recovery_hash(code: &str) -> Vec<u8> {
    let plain: String = code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    super::token_hash(&plain).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 Appendix B, SHA-1, cut to the last six of its eight digits.
    #[test]
    fn codes_match_the_rfc_vectors() {
        let secret = b"12345678901234567890";
        for (time, code) in [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
        ] {
            assert_eq!(code_at(secret, time / STEP_SECS), code, "at {time}");
        }
    }

    #[test]
    fn a_code_counts_once_and_only_near_now() {
        let secret = b"12345678901234567890";
        let now = 1_111_111_109;
        let step = now / STEP_SECS;
        assert_eq!(verify(secret, "081804", now, None), Some(step));
        assert_eq!(verify(secret, "081 804", now, None), Some(step), "spaces");
        assert_eq!(verify(secret, "081804", now, Some(step)), None, "used");
        assert_eq!(
            verify(secret, "081804", now + STEP_SECS, None),
            Some(step),
            "a step late"
        );
        assert_eq!(verify(secret, "081804", now + 3 * STEP_SECS, None), None);
        assert_eq!(verify(secret, "81804", now, None), None, "five digits");
        assert_eq!(verify(secret, "o81804", now, None), None);
    }

    #[test]
    fn base32_matches_the_rfc_vectors() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"fo"), "MZXQ");
        assert_eq!(base32(b"foo"), "MZXW6");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(&new_secret()).len(), 32);
    }

    #[test]
    fn recovery_codes_are_distinct_and_forgiving_to_type() {
        let a = recovery_code();
        assert_eq!(a.len(), 11);
        assert_ne!(a, recovery_code());
        assert_eq!(
            recovery_hash(&a),
            recovery_hash(&a.to_uppercase().replace('-', " "))
        );
    }

    #[test]
    fn a_sealed_key_opens_only_for_its_account_under_its_secret() {
        assert!(key_from("too short").is_none());
        let key = key_from(&"k".repeat(64)).expect("long enough");
        let other = key_from(&"j".repeat(64)).expect("long enough");
        let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
        let secret = new_secret();
        let sealed = seal(&key, alice, &secret);
        assert_ne!(sealed[NONCE_LEN..], secret[..], "not in the clear");
        assert_eq!(open(&key, alice, &sealed), Some(secret));
        assert_eq!(open(&key, bob, &sealed), None, "another account");
        assert_eq!(open(&other, alice, &sealed), None, "another secret");
        let mut changed = sealed.clone();
        changed[NONCE_LEN] ^= 1;
        assert_eq!(open(&key, alice, &changed), None, "tampered");
        assert_eq!(open(&key, alice, &sealed[..NONCE_LEN]), None);
    }

    #[test]
    fn the_app_address_names_the_wiki_and_the_account() {
        let uri = otpauth_uri("Filian Wiki", "vai", b"foo");
        assert!(
            uri.starts_with("otpauth://totp/Filian%20Wiki:vai?secret=MZXW6&issuer=Filian%20Wiki")
        );
        assert!(qr_svg(&uri).is_some_and(|svg| svg.contains("<svg")));
    }
}

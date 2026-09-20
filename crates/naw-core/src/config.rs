//! Configuration loading from config.toml plus environment overrides.

use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::error::AppError;

#[derive(Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_database_url")]
    pub database_url: String,
    #[serde(default = "default_valkey_url")]
    pub valkey_url: String,
    #[serde(default = "default_http_bind")]
    pub http_bind: String,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_storage_root")]
    pub storage_root: String,
    #[serde(default = "default_skin_dir")]
    pub skin_dir: String,
    #[serde(default = "default_seed_dir")]
    pub seed_dir: String,
    #[serde(default)]
    pub auth: AuthConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: default_database_url(),
            valkey_url: default_valkey_url(),
            http_bind: default_http_bind(),
            http_port: default_http_port(),
            storage_root: default_storage_root(),
            skin_dir: default_skin_dir(),
            seed_dir: default_seed_dir(),
            auth: AuthConfig::default(),
        }
    }
}

fn default_database_url() -> String {
    "postgres://naw:naw@127.0.0.1:5433/naw_dev".to_string()
}

fn default_valkey_url() -> String {
    "redis://127.0.0.1:6380".to_string()
}

fn default_http_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_http_port() -> u16 {
    4242
}

fn default_storage_root() -> String {
    "./uploads".to_string()
}

fn default_skin_dir() -> String {
    "skins/default".to_string()
}

fn default_seed_dir() -> String {
    "seeds".to_string()
}

/// OAuth, sessions and mail. A provider is enabled when its credentials
/// exist; there is no separate per-provider switch. Secrets are loaded from
/// the environment only and are redacted from `Debug` output.
#[derive(Clone, Deserialize)]
pub struct AuthConfig {
    /// Master switch. When false every auth route answers 404.
    #[serde(default)]
    pub enabled: bool,
    /// Absolute base URL used in callbacks and email links.
    pub base_url: Option<String>,
    /// Session lifetime in hours, 720 = 30 days.
    #[serde(default = "default_session_ttl_hours")]
    pub session_ttl_hours: i64,
    /// Attach an identity by provider-certified email when the switch is on.
    #[serde(default = "default_true")]
    pub auto_link_verified_email: bool,
    /// Enables the loopback-only `dev` provider for end-to-end runs.
    #[serde(default)]
    pub dev_login: bool,
    /// Serves /dev/mailbox from the log mailer, loopback only.
    #[serde(default)]
    pub dev_mailbox: bool,
    /// Usernames only an admin may grant (seed, admin, wiki, support).
    #[serde(default = "default_reserved_usernames")]
    pub reserved_usernames: Vec<String>,
    /// Round 1 through round 3 provider credentials, env supplied.
    pub github: Option<OAuth2Creds>,
    pub discord: Option<OAuth2Creds>,
    pub telegram: Option<OAuth2Creds>,
    pub google: Option<OAuth2Creds>,
    pub yandex: Option<OAuth2Creds>,
    pub twitch: Option<OAuth2Creds>,
    /// Steam keeps an API key for the profile lookup, login works without it.
    pub steam_api_key: Option<String>,
    /// Outbound mail delivery, see `MailConfig`.
    #[serde(default = "default_mail")]
    pub mail: MailConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        // Mirrors the serde defaults above. A derived Default would hand out
        // session_ttl_hours = 0 and an empty reserved list whenever
        // config.toml is missing, which silently kills every session.
        Self {
            enabled: false,
            base_url: None,
            session_ttl_hours: default_session_ttl_hours(),
            auto_link_verified_email: default_true(),
            dev_login: false,
            dev_mailbox: false,
            reserved_usernames: default_reserved_usernames(),
            github: None,
            discord: None,
            telegram: None,
            google: None,
            yandex: None,
            twitch: None,
            steam_api_key: None,
            mail: default_mail(),
        }
    }
}

fn default_session_ttl_hours() -> i64 {
    720
}

fn default_true() -> bool {
    true
}

fn default_reserved_usernames() -> Vec<String> {
    [
        "vai",
        "snacker",
        "filian",
        "admin",
        "moderator",
        "wiki",
        "support",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_mail() -> MailConfig {
    MailConfig::default()
}

#[derive(Clone, Deserialize)]
pub struct OAuth2Creds {
    pub client_id: String,
    pub client_secret: String,
}

/// `log` prints to the log and /dev/mailbox in dev, `smtp` is the real path
/// (Mailpit in compose). Receiving in tests goes through testmail.app, which
/// cannot send.
#[derive(Clone, Default, Deserialize)]
pub struct MailConfig {
    /// "log" or "smtp".
    #[serde(default)]
    pub backend: MailBackend,
    /// smtp://host:port or smtps://user:pass@host:port.
    pub smtp_url: Option<String>,
    /// Envelope from, e.g. "FilianWIKI <noreply@vai-rice.space>".
    pub mail_from: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailBackend {
    #[default]
    Log,
    Smtp,
}

/// Presence flag for `Debug` output: secrets print as set or unset.
fn creds<T>(value: &Option<T>) -> &'static str {
    if value.is_some() { "set" } else { "unset" }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Credentials live inside the URLs. Never print them.
        f.debug_struct("Config")
            .field("database_url", &"[redacted]")
            .field("valkey_url", &"[redacted]")
            .field("http_bind", &self.http_bind)
            .field("http_port", &self.http_port)
            .field("storage_root", &self.storage_root)
            .field("skin_dir", &self.skin_dir)
            .field("seed_dir", &self.seed_dir)
            .field("auth", &self.auth)
            .finish()
    }
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every secret field is collapsed to a presence flag. Debug output
        // must stay safe for logs and bug reports.
        f.debug_struct("AuthConfig")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("session_ttl_hours", &self.session_ttl_hours)
            .field("auto_link_verified_email", &self.auto_link_verified_email)
            .field("dev_login", &self.dev_login)
            .field("dev_mailbox", &self.dev_mailbox)
            .field("reserved_usernames", &self.reserved_usernames)
            .field("github", &creds(&self.github))
            .field("discord", &creds(&self.discord))
            .field("telegram", &creds(&self.telegram))
            .field("google", &creds(&self.google))
            .field("yandex", &creds(&self.yandex))
            .field("twitch", &creds(&self.twitch))
            .field("steam_api_key", &creds(&self.steam_api_key))
            .field("mail", &self.mail)
            .finish()
    }
}

impl fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The SMTP URL can embed the password. Redact like the URLs above.
        f.debug_struct("MailConfig")
            .field("backend", &self.backend)
            .field("smtp_url", &"[redacted]")
            .field("mail_from", &self.mail_from)
            .finish()
    }
}

impl Config {
    /// Loads from a TOML file, then applies env overrides. A missing file is
    /// not an error: defaults target the local compose stack.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let mut cfg: Self = match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw).map_err(|err| AppError::Config(err.to_string()))?,
            Err(_) => Self::default(),
        };
        if let Ok(value) =
            std::env::var("NAW_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
        {
            cfg.database_url = value;
        }
        if let Ok(value) = std::env::var("NAW_VALKEY_URL").or_else(|_| std::env::var("VALKEY_URL"))
        {
            cfg.valkey_url = value;
        }
        if let Ok(value) = std::env::var("NAW_HTTP_BIND") {
            cfg.http_bind = value;
        }
        if let Ok(value) = std::env::var("NAW_HTTP_PORT") {
            cfg.http_port = value
                .parse()
                .map_err(|err| AppError::Config(format!("NAW_HTTP_PORT: {err}")))?;
        }
        if let Ok(value) = std::env::var("NAW_STORAGE_ROOT") {
            cfg.storage_root = value;
        }
        if let Ok(value) = std::env::var("NAW_SKIN_DIR") {
            cfg.skin_dir = value;
        }
        if let Ok(value) = std::env::var("NAW_SEED_DIR") {
            cfg.seed_dir = value;
        }
        apply_auth_env(&mut cfg.auth);
        Ok(cfg)
    }
}

/// Applies the NAW_AUTH_* and provider env overrides, env wins over TOML.
fn apply_auth_env(auth: &mut AuthConfig) {
    fn parse_bool(raw: &str) -> Option<bool> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" | "enabled" => Some(true),
            "0" | "false" | "no" | "off" | "disabled" => Some(false),
            _ => None,
        }
    }
    if let Some(flag) = std::env::var("NAW_AUTH_ENABLED")
        .ok()
        .and_then(|raw| parse_bool(&raw))
    {
        auth.enabled = flag;
    }
    if let Ok(value) = std::env::var("NAW_AUTH_BASE_URL") {
        auth.base_url = Some(value);
    }
    if let Some(hours) = std::env::var("NAW_AUTH_SESSION_TTL_HOURS")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
    {
        auth.session_ttl_hours = hours;
    }
    if let Some(flag) = std::env::var("NAW_AUTH_AUTO_LINK_VERIFIED_EMAIL")
        .ok()
        .and_then(|raw| parse_bool(&raw))
    {
        auth.auto_link_verified_email = flag;
    }
    if let Some(flag) = std::env::var("NAW_AUTH_DEV_LOGIN")
        .ok()
        .and_then(|raw| parse_bool(&raw))
    {
        auth.dev_login = flag;
    }
    if let Some(flag) = std::env::var("NAW_DEV_MAILBOX")
        .ok()
        .and_then(|raw| parse_bool(&raw))
    {
        auth.dev_mailbox = flag;
    }
    if let Ok(raw) = std::env::var("NAW_AUTH_RESERVED_USERNAMES") {
        let names: Vec<String> = raw
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        if !names.is_empty() {
            auth.reserved_usernames = names;
        }
    }
    for (slot, prefix) in [
        (&mut auth.github, "NAW_GITHUB"),
        (&mut auth.discord, "NAW_DISCORD"),
        (&mut auth.telegram, "NAW_TELEGRAM"),
        (&mut auth.google, "NAW_GOOGLE"),
        (&mut auth.yandex, "NAW_YANDEX"),
        (&mut auth.twitch, "NAW_TWITCH"),
    ] {
        if let (Ok(id), Ok(secret)) = (
            std::env::var(format!("{prefix}_CLIENT_ID")),
            std::env::var(format!("{prefix}_CLIENT_SECRET")),
        ) && !id.trim().is_empty()
            && !secret.trim().is_empty()
        {
            *slot = Some(OAuth2Creds {
                client_id: id,
                client_secret: secret,
            });
        }
    }
    if let Ok(value) = std::env::var("NAW_STEAM_API_KEY") {
        auth.steam_api_key = Some(value);
    }
    if let Ok(raw) = std::env::var("NAW_MAIL_BACKEND") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "log" => auth.mail.backend = MailBackend::Log,
            "smtp" => auth.mail.backend = MailBackend::Smtp,
            _ => {}
        }
    }
    if let Ok(value) = std::env::var("NAW_SMTP_URL") {
        auth.mail.smtp_url = Some(value);
    }
    if let Ok(value) = std::env::var("NAW_MAIL_FROM") {
        auth.mail.mail_from = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_leaks_urls() {
        let cfg = Config::default();
        let dumped = format!("{cfg:?}");
        assert!(!dumped.contains("postgres://"));
        assert!(!dumped.contains("redis://"));
        assert!(dumped.contains("[redacted]"));
        assert_eq!(cfg.skin_dir, "skins/default");
    }

    #[test]
    fn auth_defaults_carry_the_locked_decisions() {
        let auth = AuthConfig::default();
        assert!(!auth.enabled, "auth stays off until creds exist");
        assert_eq!(auth.session_ttl_hours, 720);
        assert!(auth.auto_link_verified_email);
        assert!(!auth.dev_login);
        assert_eq!(
            auth.reserved_usernames,
            [
                "vai",
                "snacker",
                "filian",
                "admin",
                "moderator",
                "wiki",
                "support"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        );
        assert_eq!(auth.mail.backend, MailBackend::Log);
    }

    #[test]
    fn auth_debug_never_leaks_client_secrets_or_smtp_url() {
        let auth = AuthConfig {
            enabled: true,
            base_url: Some("https://snackers.vai-rice.space".to_string()),
            github: Some(OAuth2Creds {
                client_id: "Iv1.clientid".to_string(),
                client_secret: "supersecret-value".to_string(),
            }),
            mail: MailConfig {
                backend: MailBackend::Smtp,
                smtp_url: Some("smtps://user:pw@smtp.example:465".to_string()),
                mail_from: Some("noreply@vai-rice.space".to_string()),
            },
            ..AuthConfig::default()
        };
        let dumped = format!("{auth:?}");
        assert!(!dumped.contains("supersecret-value"));
        assert!(!dumped.contains("smtp://"));
        assert!(!dumped.contains("smtps://"));
        assert!(!dumped.contains("Iv1.clientid"));
        assert!(dumped.contains("set"));
    }

    #[test]
    fn reserved_usernames_env_replaces_the_default_list() {
        // SAFETY: tests run in one process, the env pair is unique to this
        // test and restored right away.
        unsafe {
            std::env::set_var("NAW_AUTH_RESERVED_USERNAMES", "root, keeper");
        }
        let mut auth = AuthConfig::default();
        apply_auth_env(&mut auth);
        unsafe {
            std::env::remove_var("NAW_AUTH_RESERVED_USERNAMES");
        }
        assert_eq!(
            auth.reserved_usernames,
            ["root", "keeper"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }
}

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
    #[serde(default = "default_locales_dir")]
    pub locales_dir: String,
    /// Seconds between checks for edited skin and locale files. 0 turns the
    /// watcher off; the admin panel can still reload on demand.
    #[serde(default = "default_reload_interval_secs")]
    pub reload_interval_secs: u64,
    /// Take the client address from `X-Real-IP` or `X-Forwarded-For` when
    /// the request comes from loopback, which is where a reverse proxy on the
    /// same host or a tunnel end connects from. Off by default: without a
    /// proxy those headers are whatever the client typed.
    #[serde(default)]
    pub trust_proxy: bool,
    /// The first owner, created or promoted when the server starts. Env only,
    /// never read from config.toml, because a file in the working directory
    /// is the wrong home for a password.
    #[serde(skip)]
    pub bootstrap_owner: Option<BootstrapOwner>,
    /// Defaults for how accounts may change. The install owner can override
    /// each value from the admin panel; these apply until they do.
    #[serde(default)]
    pub accounts: AccountPolicy,
    #[serde(default)]
    pub auth: AuthConfig,
}

/// What an account may change about itself, and for how long an old name
/// stays reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct AccountPolicy {
    /// Whether people may change their own username at all.
    pub rename_enabled: bool,
    /// Days between two changes of one account's username.
    pub rename_cooldown_days: i64,
    /// Switches former usernames off entirely: a rename frees the old name at
    /// once and old links stop finding the person. Off by default.
    pub aliases_disabled: bool,
    /// Days a former username stays reserved for its owner. 0 keeps it forever.
    pub alias_days: i64,
    /// Former usernames kept per account. The oldest goes first.
    pub max_aliases: i64,
}

impl Default for AccountPolicy {
    fn default() -> Self {
        Self {
            rename_enabled: true,
            rename_cooldown_days: 7,
            aliases_disabled: false,
            alias_days: 30,
            max_aliases: 5,
        }
    }
}

impl AccountPolicy {
    /// Pulls every value into a sane range, so a typo in an env var or a form
    /// cannot mean "rename every second" or "keep a million names".
    pub fn clamped(self) -> Self {
        Self {
            rename_enabled: self.rename_enabled,
            rename_cooldown_days: self.rename_cooldown_days.clamp(0, 3650),
            aliases_disabled: self.aliases_disabled,
            alias_days: self.alias_days.clamp(0, 3650),
            max_aliases: self.max_aliases.clamp(0, 50),
        }
    }
}

/// An account that must exist with every right on the install.
///
/// Set with NAW_BOOTSTRAP_OWNER_USERNAME plus either
/// NAW_BOOTSTRAP_OWNER_PASSWORD_FILE (a secrets file, preferred) or
/// NAW_BOOTSTRAP_OWNER_PASSWORD. The password is only used when the account has
/// none yet, so a restart never undoes a password its owner changed since.
#[derive(Clone)]
pub struct BootstrapOwner {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for BootstrapOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootstrapOwner")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

impl BootstrapOwner {
    /// Reads the three variables. Half a configuration is an error rather than
    /// silently nothing: a password without a name, or a name without a
    /// password, is somebody's typo on the way to locking themselves out.
    fn from_env() -> Result<Option<Self>, AppError> {
        let username = std::env::var("NAW_BOOTSTRAP_OWNER_USERNAME")
            .ok()
            .map(|name| name.trim().to_lowercase())
            .filter(|name| !name.is_empty());
        let from_file = match std::env::var("NAW_BOOTSTRAP_OWNER_PASSWORD_FILE") {
            Ok(path) if !path.trim().is_empty() => Some(
                std::fs::read_to_string(path.trim())
                    .map(|raw| raw.trim_end_matches(['\r', '\n']).to_string())
                    .map_err(|err| {
                        AppError::Config(format!("NAW_BOOTSTRAP_OWNER_PASSWORD_FILE: {err}"))
                    })?,
            ),
            _ => None,
        };
        let password = from_file.or_else(|| {
            std::env::var("NAW_BOOTSTRAP_OWNER_PASSWORD")
                .ok()
                .filter(|pw| !pw.is_empty())
        });
        match (username, password) {
            (Some(username), Some(password)) => Ok(Some(Self { username, password })),
            (None, None) => Ok(None),
            (Some(_), None) => Err(AppError::Config(
                "NAW_BOOTSTRAP_OWNER_USERNAME is set without NAW_BOOTSTRAP_OWNER_PASSWORD_FILE or NAW_BOOTSTRAP_OWNER_PASSWORD".to_string(),
            )),
            (None, Some(_)) => Err(AppError::Config(
                "a bootstrap owner password is set without NAW_BOOTSTRAP_OWNER_USERNAME".to_string(),
            )),
        }
    }
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
            locales_dir: default_locales_dir(),
            reload_interval_secs: default_reload_interval_secs(),
            trust_proxy: false,
            bootstrap_owner: None,
            accounts: AccountPolicy::default(),
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

/// Interface message catalogues, one TOML file per language.
fn default_locales_dir() -> String {
    "locales".to_string()
}

fn default_reload_interval_secs() -> u64 {
    2
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
    /// Whether a sign-in may create an account. `closed` is a closed alpha:
    /// providers still sign in people who already have an account, and new
    /// accounts come from an admin.
    #[serde(default)]
    pub registration: Registration,
    /// Where the "no account yet" page sends people, for example the page
    /// that explains how to apply. Nothing is shown when unset.
    pub apply_url: Option<String>,
    /// Username and password sign-in. On by default: it is how accounts an
    /// admin created get in, and how the first admin gets in at all.
    #[serde(default = "default_true")]
    pub password_login: bool,
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
            registration: Registration::default(),
            apply_url: None,
            password_login: true,
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

/// Whether signing in may create an account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Registration {
    /// Anyone who signs in with a provider gets an account.
    #[default]
    Open,
    /// Only people who already have an account get in.
    Closed,
}

impl Registration {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "closed" | "invite" | "invite-only" => Some(Self::Closed),
            _ => None,
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
            .field("locales_dir", &self.locales_dir)
            .field("reload_interval_secs", &self.reload_interval_secs)
            .field("trust_proxy", &self.trust_proxy)
            .field("bootstrap_owner", &self.bootstrap_owner)
            .field("accounts", &self.accounts)
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
            .field("registration", &self.registration)
            .field("apply_url", &self.apply_url)
            .field("password_login", &self.password_login)
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
        if let Ok(value) = std::env::var("NAW_LOCALES_DIR") {
            cfg.locales_dir = value;
        }
        if let Some(secs) = std::env::var("NAW_RELOAD_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            cfg.reload_interval_secs = secs;
        }
        if let Some(flag) = std::env::var("NAW_TRUST_PROXY")
            .ok()
            .and_then(|raw| parse_bool(&raw))
        {
            cfg.trust_proxy = flag;
        }
        apply_auth_env(&mut cfg.auth)?;
        apply_accounts_env(&mut cfg.accounts)?;
        cfg.bootstrap_owner = BootstrapOwner::from_env()?;
        cfg.check_dev_login()?;
        Ok(cfg)
    }

    /// Refuses to start with the dev login anywhere it could be reached from
    /// outside. The handler checks the peer address too, but behind a reverse
    /// proxy every request comes from loopback, so that check alone would let
    /// the whole internet sign in as the dev account. A public https base URL
    /// or a bind on a non-loopback address both mean "not a laptop".
    pub fn check_dev_login(&self) -> Result<(), AppError> {
        if !self.auth.dev_login {
            return Ok(());
        }
        let public_url = self.auth.base_url.as_deref().is_some_and(|url| {
            url.trim_start()
                .to_ascii_lowercase()
                .starts_with("https://")
        });
        let public_bind = !matches!(self.http_bind.trim(), "127.0.0.1" | "::1" | "localhost");
        if public_url || public_bind {
            return Err(AppError::Config(
                "the dev login is for local development only: it cannot run with an https base url or a non-loopback bind".to_string(),
            ));
        }
        Ok(())
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" => Some(false),
        _ => None,
    }
}

/// NAW_RENAME_ENABLED, NAW_ALIASES_DISABLED, NAW_RENAME_COOLDOWN_DAYS, NAW_ALIAS_DAYS and
/// NAW_MAX_ALIASES. A value that does not parse stops the start rather than
/// quietly leaving the default in place.
fn apply_accounts_env(policy: &mut AccountPolicy) -> Result<(), AppError> {
    if let Ok(raw) = std::env::var("NAW_ALIASES_DISABLED") {
        policy.aliases_disabled = parse_bool(&raw).ok_or_else(|| {
            AppError::Config(format!(
                "NAW_ALIASES_DISABLED must be true or false, got {raw:?}"
            ))
        })?;
    }
    if let Ok(raw) = std::env::var("NAW_RENAME_ENABLED") {
        policy.rename_enabled = parse_bool(&raw).ok_or_else(|| {
            AppError::Config(format!(
                "NAW_RENAME_ENABLED must be true or false, got {raw:?}"
            ))
        })?;
    }
    for (name, slot) in [
        ("NAW_RENAME_COOLDOWN_DAYS", &mut policy.rename_cooldown_days),
        ("NAW_ALIAS_DAYS", &mut policy.alias_days),
        ("NAW_MAX_ALIASES", &mut policy.max_aliases),
    ] {
        if let Ok(raw) = std::env::var(name) {
            *slot = raw.trim().parse::<i64>().map_err(|_| {
                AppError::Config(format!("{name} must be a whole number, got {raw:?}"))
            })?;
        }
    }
    *policy = policy.clamped();
    Ok(())
}

/// Applies the NAW_AUTH_* and provider env overrides, env wins over TOML.
fn apply_auth_env(auth: &mut AuthConfig) -> Result<(), AppError> {
    if let Ok(raw) = std::env::var("NAW_AUTH_REGISTRATION") {
        // A typo here must not quietly leave registration open.
        auth.registration = Registration::parse(&raw).ok_or_else(|| {
            AppError::Config(format!(
                "NAW_AUTH_REGISTRATION must be open or closed, got {raw:?}"
            ))
        })?;
    }
    if let Ok(value) = std::env::var("NAW_AUTH_APPLY_URL") {
        auth.apply_url = Some(value).filter(|v| !v.trim().is_empty());
    }
    if let Some(flag) = std::env::var("NAW_AUTH_PASSWORD_LOGIN")
        .ok()
        .and_then(|raw| parse_bool(&raw))
    {
        auth.password_login = flag;
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
    Ok(())
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
    fn account_policy_is_kept_in_range() {
        let wild = AccountPolicy {
            rename_enabled: true,
            rename_cooldown_days: -5,
            aliases_disabled: false,
            alias_days: 1_000_000,
            max_aliases: 900,
        }
        .clamped();
        assert_eq!(wild.rename_cooldown_days, 0);
        assert_eq!(wild.alias_days, 3650);
        assert_eq!(wild.max_aliases, 50);
        assert_eq!(AccountPolicy::default().alias_days, 30);
    }

    #[test]
    fn a_bootstrap_password_never_reaches_debug_output() {
        let owner = BootstrapOwner {
            username: "vai".to_string(),
            password: "hunter2-but-longer".to_string(),
        };
        let cfg = Config {
            bootstrap_owner: Some(owner),
            ..Config::default()
        };
        let dumped = format!("{cfg:?}");
        assert!(dumped.contains("vai"));
        assert!(!dumped.contains("hunter2"), "{dumped}");
    }

    #[test]
    fn registration_parses_strictly() {
        assert_eq!(Registration::parse("closed"), Some(Registration::Closed));
        assert_eq!(Registration::parse(" Invite "), Some(Registration::Closed));
        assert_eq!(Registration::parse("OPEN"), Some(Registration::Open));
        assert_eq!(Registration::parse("clsoed"), None, "a typo must not pass");
        assert_eq!(AuthConfig::default().registration, Registration::Open);
        assert!(AuthConfig::default().password_login);
    }

    #[test]
    fn dev_login_refuses_anything_that_looks_public() {
        let mut cfg = Config::default();
        cfg.auth.dev_login = true;
        cfg.http_bind = "127.0.0.1".to_string();
        cfg.auth.base_url = Some("http://127.0.0.1:4242".to_string());
        assert!(cfg.check_dev_login().is_ok(), "a laptop is fine");
        cfg.auth.base_url = Some("HTTPS://filian.wiki".to_string());
        assert!(cfg.check_dev_login().is_err(), "https base url");
        cfg.auth.base_url = None;
        cfg.http_bind = "0.0.0.0".to_string();
        assert!(cfg.check_dev_login().is_err(), "public bind");
        cfg.auth.dev_login = false;
        assert!(cfg.check_dev_login().is_ok(), "off is always fine");
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
        apply_auth_env(&mut auth).expect("env applies");
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

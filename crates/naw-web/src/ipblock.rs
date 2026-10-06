//! Address blocks, as on Wikipedia: a blocked address reads the wiki like
//! anyone, and may not write to it. Admins add them under Admin, Blocks.
//!
//! Who is held: everyone below moderator, signed in or not, curators
//! included. Moderators, admins, owners, staff and the operator are not, so a
//! moderator on a shared address keeps working.
//!
//! What counts as writing: every request that is not a GET or HEAD, except
//! signing in and out and picking a language, which change nothing on the
//! wiki and let a moderator on a blocked address get to work. A new account
//! from a blocked address is refused at the provider callback too, since that
//! one is a GET.
//!
//! The list is kept in memory and read again every half minute, or at once on
//! this process after an admin changes it; a request from an address no block
//! covers costs no database work.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use ipnetwork::IpNetwork;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::auth::session::CurrentUser;
use crate::pages;
use crate::perm::{GlobalRole, WikiRole};

/// One block as the check needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub wiki_id: Uuid,
    pub network: IpNetwork,
    pub expires_at: Option<DateTime<Utc>>,
}

/// The widest ranges a block may take. Anything wider would hold back whole
/// providers or countries by one mistyped prefix.
const MIN_PREFIX_V4: u8 = 16;
const MIN_PREFIX_V6: u8 = 32;

/// How long the kept list is trusted before it is read again.
const REFRESH: Duration = Duration::from_secs(30);

struct Kept {
    at: Instant,
    blocks: Arc<Vec<Block>>,
}

static KEPT: RwLock<Option<Kept>> = RwLock::new(None);

/// Every block, including expired ones, which [`covering`] skips.
pub async fn load(db: &sqlx::PgPool) -> Result<Vec<Block>, AppError> {
    let rows = sqlx::query!(
        r#"SELECT wiki_id, network::text AS "network!", expires_at FROM ip_blocks
           WHERE expires_at IS NULL OR expires_at > now()"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(Block {
                wiki_id: row.wiki_id,
                network: row.network.parse().ok()?,
                expires_at: row.expires_at,
            })
        })
        .collect())
}

/// The kept list, read again when it is older than [`REFRESH`]. A failed read
/// keeps the old list rather than dropping every block.
async fn current(db: &sqlx::PgPool) -> Arc<Vec<Block>> {
    if let Ok(kept) = KEPT.read()
        && let Some(kept) = kept.as_ref()
        && kept.at.elapsed() < REFRESH
    {
        return Arc::clone(&kept.blocks);
    }
    match load(db).await {
        Ok(blocks) => {
            let blocks = Arc::new(blocks);
            if let Ok(mut kept) = KEPT.write() {
                *kept = Some(Kept {
                    at: Instant::now(),
                    blocks: Arc::clone(&blocks),
                });
            }
            blocks
        }
        Err(err) => {
            tracing::error!(error = %err, "address blocks could not be read");
            KEPT.read()
                .ok()
                .and_then(|kept| kept.as_ref().map(|k| Arc::clone(&k.blocks)))
                .unwrap_or_default()
        }
    }
}

/// Drops the kept list, so the next check reads the database.
fn forget() {
    if let Ok(mut kept) = KEPT.write() {
        *kept = None;
    }
}

/// The block holding `ip`, on `wiki` when given, else on any wiki.
pub fn covering(
    blocks: &[Block],
    wiki: Option<Uuid>,
    ip: IpAddr,
    now: DateTime<Utc>,
) -> Option<&Block> {
    // An IPv4 client may arrive as ::ffff:a.b.c.d; blocks are written as IPv4.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    blocks.iter().find(|block| {
        wiki.is_none_or(|id| id == block.wiki_id)
            && block.expires_at.is_none_or(|end| end > now)
            && block.network.contains(ip)
    })
}

/// Whether a block holds this reader: everyone below moderator.
pub fn held(role: Option<WikiRole>, global: GlobalRole) -> bool {
    global != GlobalRole::Root && role.is_none_or(|role| role < WikiRole::Moderator)
}

/// Requests a blocked address may still send.
fn is_allowed_write(path: &str) -> bool {
    path == "/logout"
        || path == "/lang"
        || path == "/settings/language"
        || path == "/discord/interactions"
        || path == "/login"
        || path.starts_with("/login/")
}

/// Why an address cannot be blocked as written.
#[derive(Debug, PartialEq, Eq)]
pub enum Unusable {
    /// Not an address or a range.
    Unreadable,
    /// Wider than [`MIN_PREFIX_V4`] or [`MIN_PREFIX_V6`].
    TooWide,
    /// Loopback, unspecified or private: behind the proxy these are never a
    /// reader, and blocking one could hold back everyone.
    Local,
}

/// Reads `1.2.3.4`, `1.2.3.0/24` or an IPv6 form, with host bits cleared.
pub fn parse_network(raw: &str) -> Result<IpNetwork, Unusable> {
    let network: IpNetwork = raw.trim().parse().map_err(|_| Unusable::Unreadable)?;
    let prefix = network.prefix();
    let ip = network.network();
    let local = match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_unspecified() || v4.is_private() || v4.is_link_local()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    };
    if local {
        return Err(Unusable::Local);
    }
    let too_wide = match ip {
        IpAddr::V4(_) => prefix < MIN_PREFIX_V4,
        IpAddr::V6(_) => prefix < MIN_PREFIX_V6,
    };
    if too_wide {
        return Err(Unusable::TooWide);
    }
    IpNetwork::new(ip, prefix).map_err(|_| Unusable::Unreadable)
}

/// The middleware. Runs inside the session and the error pages, so it knows
/// who is asking and its refusal is a themed page.
pub async fn layer(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    let method = req.method();
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        return next.run(req).await;
    }
    if is_allowed_write(req.uri().path()) {
        return next.run(req).await;
    }
    let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied() else {
        return next.run(req).await;
    };
    let ip = crate::net::client_ip(req.headers(), peer, state.config.trust_proxy);
    let blocks = current(&state.db).await;
    if covering(&blocks, None, ip, Utc::now()).is_none() {
        return next.run(req).await;
    }
    // A block somewhere covers this address: find out whether on this wiki,
    // and whether this reader is one it holds.
    let user = req
        .extensions()
        .get::<Option<CurrentUser>>()
        .cloned()
        .flatten();
    let ctx = match crate::resolve::context(&state, req.headers(), user.as_ref()).await {
        Ok(Some(ctx)) => ctx,
        _ => return next.run(req).await,
    };
    let Some(block) = covering(&blocks, Some(ctx.wiki.id), ip, Utc::now()) else {
        return next.run(req).await;
    };
    if !held(ctx.actor.effective_role(), ctx.actor.global) {
        return next.run(req).await;
    }
    tracing::info!(%ip, path = req.uri().path(), "write from a blocked address refused");
    refusal(&ctx, block)
}

/// The 403 a blocked reader sees; the error layer puts it in a page.
fn refusal(ctx: &crate::resolve::Ctx, block: &Block) -> Response {
    let text = match block.expires_at {
        Some(end) => ctx.t_with(
            "ipblock.refused_until",
            &[("until", &end.format("%Y-%m-%d %H:%M UTC").to_string())],
        ),
        None => ctx.t("ipblock.refused"),
    };
    (StatusCode::FORBIDDEN, text).into_response()
}

/// Whether a new account may be made from this request: `false` when a block
/// on this wiki holds the address. A sign-in to an existing account is not
/// asked about, only a registration.
pub async fn may_register(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> bool {
    let ip = crate::net::client_ip(headers, peer, state.config.trust_proxy);
    let blocks = current(&state.db).await;
    if covering(&blocks, None, ip, Utc::now()).is_none() {
        return true;
    }
    match crate::resolve::context(state, headers, None).await {
        Ok(Some(ctx)) => covering(&blocks, Some(ctx.wiki.id), ip, Utc::now()).is_none(),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Admin, Blocks
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct DoneQuery {
    #[serde(default)]
    done: Option<String>,
}

/// GET /admin/blocks
pub async fn admin_page(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<DoneQuery>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let rows = sqlx::query!(
        r#"SELECT b.id, b.network::text AS "network!", b.reason, b.created_at, b.expires_at,
                  u.username AS "by?"
           FROM ip_blocks b LEFT JOIN users u ON u.id = b.created_by
           WHERE b.wiki_id = $1
           ORDER BY (b.expires_at IS NOT NULL AND b.expires_at <= now()), b.created_at DESC"#,
        ctx.wiki.id
    )
    .fetch_all(&state.db)
    .await?;
    let now = Utc::now();
    let blocks: Vec<minijinja::Value> = rows
        .into_iter()
        .map(|row| {
            minijinja::context! {
                id => row.id.to_string(),
                // A single address reads without its /32 or /128.
                network => row.network.strip_suffix("/32").or_else(|| row.network.strip_suffix("/128")).unwrap_or(&row.network).to_string(),
                reason => row.reason,
                by => row.by,
                since => ctx.day(row.created_at),
                until => row.expires_at.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()),
                expired => row.expires_at.is_some_and(|t| t <= now),
            }
        })
        .collect();
    let mine = crate::net::client_ip(&headers, peer, state.config.trust_proxy);
    crate::admin::render(
        &ctx,
        "blocks",
        &ctx.t("ipblock.title"),
        minijinja::context! {
            blocks => blocks,
            my_address => mine.to_string(),
            durations => DURATIONS.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            done => pages::message_key(query.done.as_deref(), &[""]),
        },
    )
}

/// The lengths a block can be given, the form's choices.
const DURATIONS: &[(&str, Option<i64>)] = &[
    ("day", Some(1)),
    ("week", Some(7)),
    ("month", Some(30)),
    ("year", Some(365)),
    ("forever", None),
];

#[derive(Deserialize)]
pub struct AddForm {
    network: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    duration: String,
}

/// Longest reason kept, in characters.
const REASON_CHARS: usize = 500;

/// POST /admin/blocks/add
pub async fn add(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    headers: HeaderMap,
    Form(form): Form<AddForm>,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let network = match parse_network(&form.network) {
        Ok(network) => network,
        Err(Unusable::Unreadable) => return Ok(pages::see_other("/admin/blocks?done=unreadable")),
        Err(Unusable::TooWide) => return Ok(pages::see_other("/admin/blocks?done=too_wide")),
        Err(Unusable::Local) => return Ok(pages::see_other("/admin/blocks?done=local")),
    };
    let days = DURATIONS
        .iter()
        .find(|(id, _)| *id == form.duration)
        .map(|(_, days)| *days)
        .unwrap_or(None);
    let expires_at = days.map(|d| Utc::now() + chrono::Duration::days(d));
    let reason: String = form.reason.trim().chars().take(REASON_CHARS).collect();
    let id = Uuid::new_v4();
    // Blocking an address again replaces its length and reason.
    sqlx::query!(
        "INSERT INTO ip_blocks (id, wiki_id, network, reason, created_by, expires_at)
         VALUES ($1, $2, $3::text::cidr, $4, $5, $6)
         ON CONFLICT (wiki_id, network) DO UPDATE
         SET reason = EXCLUDED.reason, created_by = EXCLUDED.created_by,
             created_at = now(), expires_at = EXCLUDED.expires_at",
        id,
        ctx.wiki.id,
        network.to_string(),
        reason,
        ctx.actor.user_id,
        expires_at
    )
    .execute(&state.db)
    .await?;
    forget();
    crate::audit::record_or_log(
        &state.db,
        crate::audit::Entry {
            wiki_id: Some(ctx.wiki.id),
            user_id: ctx.actor.user_id,
            action: "ipblock.add",
            entity_type: "ip_block",
            entity_id: None,
            meta: json!({
                "network": network.to_string(),
                "reason": reason,
                "expires_at": expires_at.map(|t| t.to_rfc3339()),
            }),
        },
    )
    .await;
    Ok(pages::see_other("/admin/blocks?done=added"))
}

/// POST /admin/blocks/{id}/remove
pub async fn remove(
    State(state): State<AppState>,
    Extension(user): Extension<Option<CurrentUser>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ctx = or_respond!(crate::admin::gate(&state, &headers, user.as_ref()).await);
    let removed = sqlx::query!(
        r#"DELETE FROM ip_blocks WHERE id = $1 AND wiki_id = $2
           RETURNING network::text AS "network!", reason"#,
        id,
        ctx.wiki.id
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(row) = removed {
        forget();
        crate::audit::record_or_log(
            &state.db,
            crate::audit::Entry {
                wiki_id: Some(ctx.wiki.id),
                user_id: ctx.actor.user_id,
                action: "ipblock.remove",
                entity_type: "ip_block",
                entity_id: Some(id),
                meta: json!({ "network": row.network, "reason": row.reason }),
            },
        )
        .await;
    }
    Ok(pages::see_other("/admin/blocks?done=removed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    fn ip(raw: &str) -> IpAddr {
        raw.parse().unwrap()
    }

    fn block(wiki: Uuid, network: &str, expires_at: Option<DateTime<Utc>>) -> Block {
        Block {
            wiki_id: wiki,
            network: network.parse().unwrap(),
            expires_at,
        }
    }

    #[test]
    fn a_block_covers_its_address_or_range_on_its_wiki() {
        let wiki = Uuid::new_v4();
        let other = Uuid::new_v4();
        let now = Utc::now();
        let blocks = vec![
            block(wiki, "75.50.80.26/32", None),
            block(wiki, "198.51.100.0/24", None),
        ];
        assert!(covering(&blocks, Some(wiki), ip("75.50.80.26"), now).is_some());
        assert!(
            covering(&blocks, Some(wiki), ip("::ffff:75.50.80.26"), now).is_some(),
            "mapped IPv4"
        );
        assert!(covering(&blocks, Some(wiki), ip("75.50.80.27"), now).is_none());
        assert!(
            covering(&blocks, Some(wiki), ip("198.51.100.200"), now).is_some(),
            "in the range"
        );
        assert!(
            covering(&blocks, Some(other), ip("75.50.80.26"), now).is_none(),
            "another wiki"
        );
        assert!(
            covering(&blocks, None, ip("75.50.80.26"), now).is_some(),
            "any wiki"
        );
    }

    #[test]
    fn an_expired_block_holds_no_one() {
        let wiki = Uuid::new_v4();
        let now = Utc::now();
        let blocks = vec![block(
            wiki,
            "203.0.113.5/32",
            Some(now - chrono::Duration::minutes(1)),
        )];
        assert!(covering(&blocks, Some(wiki), ip("203.0.113.5"), now).is_none());
        let blocks = vec![block(
            wiki,
            "203.0.113.5/32",
            Some(now + chrono::Duration::days(1)),
        )];
        assert!(covering(&blocks, Some(wiki), ip("203.0.113.5"), now).is_some());
    }

    #[test]
    fn moderators_and_up_are_not_held() {
        assert!(held(None, GlobalRole::Registered), "a guest");
        assert!(held(Some(WikiRole::Registered), GlobalRole::Registered));
        assert!(held(Some(WikiRole::Sponsor), GlobalRole::Registered));
        assert!(
            held(Some(WikiRole::Curator), GlobalRole::Registered),
            "curators are held"
        );
        assert!(!held(Some(WikiRole::Moderator), GlobalRole::Registered));
        assert!(!held(Some(WikiRole::Admin), GlobalRole::Registered));
        assert!(!held(Some(WikiRole::Owner), GlobalRole::Registered));
        assert!(!held(None, GlobalRole::Root), "the operator");
    }

    #[test]
    fn signing_in_and_out_stays_open() {
        for path in [
            "/login",
            "/login/password",
            "/login/relay/start",
            "/logout",
            "/lang",
            "/settings/language",
        ] {
            assert!(is_allowed_write(path), "{path}");
        }
        for path in [
            "/filian/edit",
            "/preview",
            "/drafts/save",
            "/media/upload",
            "/settings/display-name",
            "/loginx",
        ] {
            assert!(!is_allowed_write(path), "{path}");
        }
    }

    #[test]
    fn only_sensible_networks_can_be_blocked() {
        assert_eq!(
            parse_network(" 75.50.80.26 ").unwrap().to_string(),
            "75.50.80.26/32"
        );
        assert_eq!(
            parse_network("198.51.100.77/24").unwrap().to_string(),
            "198.51.100.0/24",
            "host bits cleared"
        );
        assert_eq!(
            parse_network("2001:db8::1/64").unwrap().to_string(),
            "2001:db8::/64"
        );
        assert_eq!(parse_network("75.0.0.0/8"), Err(Unusable::TooWide));
        assert_eq!(parse_network("2001:db8::/16"), Err(Unusable::TooWide));
        assert_eq!(parse_network("127.0.0.1"), Err(Unusable::Local));
        assert_eq!(parse_network("192.168.31.32"), Err(Unusable::Local));
        assert_eq!(parse_network("::1"), Err(Unusable::Local));
        assert_eq!(parse_network("not an address"), Err(Unusable::Unreadable));
        assert_eq!(parse_network(""), Err(Unusable::Unreadable));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn blocks_round_trip_through_the_database(db: PgPool) {
        let wiki = Uuid::new_v4();
        sqlx::query("INSERT INTO wikis (id, slug, name, domain) VALUES ($1, 'w', 'W', 'w.test')")
            .bind(wiki)
            .execute(&db)
            .await
            .expect("wiki");
        for (network, offset) in [
            ("75.50.80.26/32", None),
            ("198.51.100.0/24", Some("1 day")),
            ("203.0.113.9/32", Some("-1 day")),
        ] {
            sqlx::query(
                "INSERT INTO ip_blocks (id, wiki_id, network, expires_at)
                 VALUES ($1, $2, $3::cidr, now() + $4::text::interval)",
            )
            .bind(Uuid::new_v4())
            .bind(wiki)
            .bind(network)
            .bind(offset)
            .execute(&db)
            .await
            .expect("block");
        }
        let blocks = load(&db).await.expect("load");
        assert_eq!(blocks.len(), 2, "the expired one is left out");
        let now = Utc::now();
        assert!(covering(&blocks, Some(wiki), ip("75.50.80.26"), now).is_some());
        assert!(covering(&blocks, Some(wiki), ip("198.51.100.4"), now).is_some());
        assert!(covering(&blocks, Some(wiki), ip("203.0.113.9"), now).is_none());
    }
}

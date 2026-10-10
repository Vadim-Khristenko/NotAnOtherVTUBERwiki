//! Rules for new accounts, against spam on an open wiki.
//!
//! An account is new until it is `newcomer_days` old and has
//! `newcomer_edits` accepted edits on the wiki, the way Wikipedia's
//! autoconfirmed works. A new account waits a few minutes before its first
//! edit, may add only a few outside links per edit, and may start only a few
//! pages a day. Whoever holds the review pass (curators and up, and anyone it
//! was given to) is never held, and `newcomer_days = 0` turns all of it off.
//! Each limit is in [`naw_core::limits`], set per wiki.

use uuid::Uuid;

use naw_core::error::AppError;
use naw_core::state::AppState;

use crate::perm::Capability;
use crate::resolve::Ctx;

/// What is being saved.
#[derive(Clone, Copy)]
pub(crate) enum Change<'a> {
    /// A new page.
    Create { body: &'a str },
    /// A new revision over `old`.
    Edit { old: &'a str, body: &'a str },
}

/// The reason to refuse this change, in the reader's language, or `None`.
pub(crate) async fn refusal(
    state: &AppState,
    ctx: &Ctx,
    change: Change<'_>,
) -> Result<Option<String>, AppError> {
    let limits = &ctx.limits;
    let Some(user_id) = ctx.actor.user_id else {
        return Ok(None);
    };
    if limits.newcomer_days == 0 || ctx.actor.can(Capability::EditUnreviewed) {
        return Ok(None);
    }
    let standing = standing(state, ctx.wiki.id, user_id).await?;
    Ok(decide(limits, &standing, change).map(|held| match held {
        Held::Wait { minutes } => ctx.t_with(
            "newcomer.first_edit_wait",
            &[("minutes", &minutes.to_string())],
        ),
        Held::Links { count, limit } => ctx.t_with(
            "newcomer.too_many_links",
            &[("count", &count.to_string()), ("limit", &limit.to_string())],
        ),
        Held::Pages { limit } => {
            ctx.t_with("newcomer.too_many_pages", &[("limit", &limit.to_string())])
        }
    }))
}

/// Why a change is held back.
#[derive(Debug, PartialEq)]
enum Held {
    Wait { minutes: i64 },
    Links { count: i64, limit: i64 },
    Pages { limit: i64 },
}

/// The rules themselves, for an account that is neither exempt nor off.
fn decide(
    limits: &naw_core::limits::Limits,
    standing: &Standing,
    change: Change<'_>,
) -> Option<Held> {
    let new = standing.age_minutes < limits.newcomer_days * 24 * 60
        || standing.accepted_edits < limits.newcomer_edits;
    if !new {
        return None;
    }
    if standing.accepted_edits == 0 && standing.age_minutes < limits.newcomer_first_edit_minutes {
        return Some(Held::Wait {
            minutes: (limits.newcomer_first_edit_minutes - standing.age_minutes).max(1),
        });
    }
    let (old, body) = match change {
        Change::Create { body } => ("", body),
        Change::Edit { old, body } => (old, body),
    };
    let count = new_outside_links(old, body);
    if count > limits.newcomer_links_per_edit {
        return Some(Held::Links {
            count,
            limit: limits.newcomer_links_per_edit,
        });
    }
    if matches!(change, Change::Create { .. })
        && standing.pages_today >= limits.newcomer_new_pages_per_day
    {
        return Some(Held::Pages {
            limit: limits.newcomer_new_pages_per_day,
        });
    }
    None
}

struct Standing {
    age_minutes: i64,
    accepted_edits: i64,
    pages_today: i64,
}

async fn standing(state: &AppState, wiki_id: Uuid, user_id: Uuid) -> Result<Standing, AppError> {
    let row = sqlx::query!(
        r#"SELECT
             (EXTRACT(EPOCH FROM now() - u.created_at) / 60)::bigint AS "age_minutes!",
             (SELECT count(*) FROM revisions r JOIN pages p ON p.id = r.page_id
               WHERE p.wiki_id = $1 AND r.author_id = $2 AND r.review_status = 'accepted') AS "accepted_edits!",
             (SELECT count(*) FROM pages p
               WHERE p.wiki_id = $1 AND p.created_at > now() - interval '1 day'
                 AND (SELECT r.author_id FROM revisions r WHERE r.page_id = p.id
                      ORDER BY r.created_at LIMIT 1) = $2) AS "pages_today!"
           FROM users u WHERE u.id = $2"#,
        wiki_id,
        user_id
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Standing {
        age_minutes: row.age_minutes,
        accepted_edits: row.accepted_edits,
        pages_today: row.pages_today,
    })
}

/// How many outside links `body` has that `old` does not: each address
/// counted once, so moving or repeating a link costs nothing.
fn new_outside_links(old: &str, body: &str) -> i64 {
    let before = outside_links(old);
    let mut seen = std::collections::HashSet::new();
    outside_links(body)
        .into_iter()
        .filter(|link| !before.contains(link) && seen.insert(link.clone()))
        .count() as i64
}

/// Every `http://` and `https://` address in the text, lowercased host and
/// all, up to the first character that cannot be part of it.
fn outside_links(text: &str) -> std::collections::HashSet<String> {
    let mut found = std::collections::HashSet::new();
    let lower = text.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("http") {
        let tail = &rest[at..];
        let scheme = if tail.starts_with("https://") {
            8
        } else if tail.starts_with("http://") {
            7
        } else {
            rest = &rest[at + 4..];
            continue;
        };
        let end = tail[scheme..]
            .find(|c: char| c.is_whitespace() || "<>\"')]|`".contains(c))
            .map_or(tail.len(), |e| scheme + e);
        let link = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if link.len() > scheme {
            found.insert(link.to_string());
        }
        rest = &tail[end..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_added_addresses_count_once_each() {
        let old = "See https://twitch.tv/filian and [VOD](https://youtu.be/a).";
        let body = "See https://twitch.tv/filian, [VOD](https://youtu.be/a) and \
                    <https://spam.example/x> plus https://spam.example/x again and http://two.example.";
        assert_eq!(new_outside_links(old, body), 2);
        assert_eq!(new_outside_links("", "no links here, just http talk"), 0);
        assert_eq!(new_outside_links("", "[a](HTTPS://Example.com/A)"), 1);
    }

    fn standing(age_minutes: i64, accepted_edits: i64, pages_today: i64) -> Standing {
        Standing {
            age_minutes,
            accepted_edits,
            pages_today,
        }
    }

    #[test]
    fn the_rules_hold_new_accounts_and_let_settled_ones_through() {
        let limits = naw_core::limits::Limits::default();
        let links = "https://a.example https://b.example https://c.example https://d.example";
        let create = Change::Create { body: links };
        // a minute old, no edits yet: wait
        assert_eq!(
            decide(&limits, &standing(1, 0, 0), create),
            Some(Held::Wait { minutes: 4 })
        );
        // an hour old: four new links is one too many
        assert_eq!(
            decide(&limits, &standing(60, 0, 0), create),
            Some(Held::Links { count: 4, limit: 3 })
        );
        // links already in the page cost nothing
        let edit = Change::Edit {
            old: links,
            body: links,
        };
        assert_eq!(decide(&limits, &standing(60, 0, 0), edit), None);
        // a fourth page today
        let page = Change::Create { body: "text" };
        assert_eq!(
            decide(&limits, &standing(60, 2, 3), page),
            Some(Held::Pages { limit: 3 })
        );
        // old enough with enough edits: no rules
        let week = 7 * 24 * 60;
        assert_eq!(decide(&limits, &standing(week, 10, 9), create), None);
        // old but with few edits is still new
        assert!(decide(&limits, &standing(week, 2, 0), create).is_some());
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_address() {
        let links = outside_links("Go to https://a.example/x. Or (https://b.example/y), fine?");
        assert!(links.contains("https://a.example/x"));
        assert!(links.contains("https://b.example/y"));
        assert_eq!(links.len(), 2);
    }
}

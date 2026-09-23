//! Who may do what.
//!
//! An install-wide role (`users.global_role`), a per-wiki role
//! (`wiki_memberships.role`) and per-wiki switches in
//! `wikis.settings.permissions`. By default a guest can only read. Everything
//! is a pure function over resolved roles except [`resolve`].

use serde_json::Value;
use uuid::Uuid;

use naw_core::error::AppError;

use crate::auth::session::CurrentUser;

/// One thing a request can be allowed to do. Deliberately coarse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// Create a page that does not exist yet.
    PageCreate,
    /// Save a new revision over an existing page, reverts included.
    PageEdit,
    /// Archive a page, or restore an archived one.
    PageDelete,
    /// Protect a page against edits, or lift it.
    PageLock,
    /// Mark a revision as checked.
    RevisionPatrol,
    /// Read the audit log.
    AuditRead,
    /// Reach the admin panel.
    AdminPanel,
    /// Change what another account may do.
    UserRoleManage,
    /// Change a wiki's name, locale, home page and switches.
    WikiSettings,
}

impl Capability {
    /// Stable identifier for audit rows and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PageCreate => "page.create",
            Self::PageEdit => "page.edit",
            Self::PageDelete => "page.delete",
            Self::PageLock => "page.lock",
            Self::RevisionPatrol => "revision.patrol",
            Self::AuditRead => "audit.read",
            Self::AdminPanel => "admin.panel",
            Self::UserRoleManage => "user.role",
            Self::WikiSettings => "wiki.settings",
        }
    }

    pub const ALL: [Capability; 9] = [
        Self::PageCreate,
        Self::PageEdit,
        Self::PageDelete,
        Self::PageLock,
        Self::RevisionPatrol,
        Self::AuditRead,
        Self::AdminPanel,
        Self::UserRoleManage,
        Self::WikiSettings,
    ];

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cap| cap.as_str() == raw)
    }

    /// Whether this writes to the wiki; a mute or ban removes it regardless.
    pub fn is_write(self) -> bool {
        matches!(
            self,
            Self::PageCreate
                | Self::PageEdit
                | Self::PageDelete
                | Self::PageLock
                | Self::RevisionPatrol
        )
    }

    /// Only an owner may grant these individually: each changes who may do what.
    pub fn owner_only(self) -> bool {
        matches!(
            self,
            Self::AdminPanel | Self::UserRoleManage | Self::WikiSettings
        )
    }
}

/// An active sanction on this wiki.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sanction {
    /// May read, may not publish.
    Mute,
    /// May read and nothing else.
    Ban,
}

/// Per-wiki role, mirroring the `user_wiki_role` enum. Declaration order is
/// the privilege order. `Sponsor` is a visible tier with no extra capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WikiRole {
    Registered,
    Sponsor,
    /// A trusted editor: protects pages up to curator level, checks edits, and
    /// edits the profiles of the people assigned to them.
    Curator,
    Moderator,
    Admin,
    Owner,
}

impl WikiRole {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "registered" => Some(Self::Registered),
            "sponsor" => Some(Self::Sponsor),
            "curator" => Some(Self::Curator),
            "moderator" => Some(Self::Moderator),
            "admin" => Some(Self::Admin),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Sponsor => "sponsor",
            Self::Curator => "curator",
            Self::Moderator => "moderator",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }

    /// The roles an admin panel may offer, weakest first.
    pub const ALL: [Self; 6] = [
        Self::Registered,
        Self::Sponsor,
        Self::Curator,
        Self::Moderator,
        Self::Admin,
        Self::Owner,
    ];
}

/// Install-wide role, mirroring `users.global_role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalRole {
    /// Privileges come from per-wiki membership only.
    Registered,
    /// Moderator level on every wiki of the install.
    Staff,
    /// The operator: every capability everywhere.
    Root,
}

impl GlobalRole {
    /// Unknown values read as the least privileged role.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "root" => Self::Root,
            "staff" => Self::Staff,
            _ => Self::Registered,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Staff => "staff",
            Self::Registered => "registered",
        }
    }
}

/// Per-wiki switches from `wikis.settings.permissions`. An absent key takes
/// the conservative default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub anonymous_create: bool,
    pub anonymous_edit: bool,
    pub registered_create: bool,
    pub registered_edit: bool,
    /// Require a verified email before any write. Off by default, since some
    /// providers share no address.
    pub require_verified_email: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            anonymous_create: false,
            anonymous_edit: false,
            registered_create: true,
            registered_edit: true,
            require_verified_email: false,
        }
    }
}

impl Rules {
    /// Reads the switches; a malformed value falls back to that key's default,
    /// never widening access.
    pub fn from_settings(settings: &Value) -> Self {
        let d = Self::default();
        let Some(block) = settings.get("permissions") else {
            return d;
        };
        let flag =
            |key: &str, fallback: bool| block.get(key).and_then(Value::as_bool).unwrap_or(fallback);
        Self {
            anonymous_create: flag("anonymous_create", d.anonymous_create),
            anonymous_edit: flag("anonymous_edit", d.anonymous_edit),
            registered_create: flag("registered_create", d.registered_create),
            registered_edit: flag("registered_edit", d.registered_edit),
            require_verified_email: flag("require_verified_email", d.require_verified_email),
        }
    }
}

/// One request's authority over one wiki.
#[derive(Clone, Debug)]
pub struct Actor {
    /// `None` for an anonymous visitor.
    pub user_id: Option<Uuid>,
    pub username: Option<String>,
    /// The name the chrome shows, when set.
    pub display_name: Option<String>,
    /// The avatar's address, when set.
    pub avatar_url: Option<String>,
    pub email_verified: bool,
    pub global: GlobalRole,
    /// The membership row, if any.
    pub membership: Option<WikiRole>,
    pub rules: Rules,
    /// Individual grants and denials on this wiki, over the role.
    pub overrides: Vec<(Capability, bool)>,
    /// The strongest active sanction on this wiki.
    pub sanction: Option<Sanction>,
}

impl Actor {
    /// The anonymous visitor, also the safe value when no wiki resolves.
    pub fn anonymous(rules: Rules) -> Self {
        Self {
            user_id: None,
            username: None,
            display_name: None,
            avatar_url: None,
            email_verified: false,
            global: GlobalRole::Registered,
            membership: None,
            rules,
            overrides: Vec::new(),
            sanction: None,
        }
    }

    pub fn is_signed_in(&self) -> bool {
        self.user_id.is_some()
    }

    /// The role for ladder checks: `Registered` for a signed-in visitor without
    /// a membership, at least moderator for staff.
    pub fn effective_role(&self) -> Option<WikiRole> {
        let base = self
            .user_id
            .map(|_| self.membership.unwrap_or(WikiRole::Registered))?;
        Some(match self.global {
            GlobalRole::Root => WikiRole::Owner,
            GlobalRole::Staff => base.max(WikiRole::Moderator),
            GlobalRole::Registered => base,
        })
    }

    fn at_least(&self, floor: WikiRole) -> bool {
        self.effective_role().is_some_and(|role| role >= floor)
    }

    /// Anonymous and signed-in writes follow their switches, but a moderator is
    /// never locked out of their own wiki by one.
    fn may_write(&self, anonymous_allowed: bool, registered_allowed: bool) -> bool {
        if !self.is_signed_in() {
            return anonymous_allowed;
        }
        if self.rules.require_verified_email && !self.email_verified {
            return false;
        }
        self.at_least(WikiRole::Moderator) || registered_allowed
    }

    pub fn can(&self, cap: Capability) -> bool {
        if self.global == GlobalRole::Root {
            return true;
        }
        // Sanctions first: no override survives a mute or a ban.
        match self.sanction {
            Some(Sanction::Ban) => return false,
            Some(Sanction::Mute) if cap.is_write() => return false,
            _ => {}
        }
        if let Some((_, allowed)) = self.overrides.iter().find(|(c, _)| *c == cap) {
            return *allowed;
        }
        self.can_by_role(cap)
    }

    /// What the role alone allows, before sanctions and overrides.
    pub fn can_by_role(&self, cap: Capability) -> bool {
        if self.global == GlobalRole::Root {
            return true;
        }
        match cap {
            Capability::PageCreate => {
                self.may_write(self.rules.anonymous_create, self.rules.registered_create)
            }
            Capability::PageEdit => {
                self.may_write(self.rules.anonymous_edit, self.rules.registered_edit)
            }
            // How high a curator may protect is limited in `may_protect`.
            Capability::PageLock | Capability::RevisionPatrol => self.at_least(WikiRole::Curator),
            Capability::PageDelete | Capability::AuditRead => self.at_least(WikiRole::Moderator),
            Capability::AdminPanel | Capability::WikiSettings | Capability::UserRoleManage => {
                self.at_least(WikiRole::Admin)
            }
        }
    }

    /// Editing one page: the capability, plus its protection level or above.
    pub fn can_edit_page(&self, protection: Option<WikiRole>) -> bool {
        if !self.can(Capability::PageEdit) {
            return false;
        }
        protection.is_none_or(|level| self.at_least(level))
    }

    /// Whether this actor may change a page's protection from `from` to `to`:
    /// never above their own role, never loosening a protection set above them.
    pub fn may_protect(&self, from: Option<WikiRole>, to: Option<WikiRole>) -> bool {
        if !self.can(Capability::PageLock) {
            return false;
        }
        from.is_none_or(|level| self.at_least(level)) && to.is_none_or(|level| self.at_least(level))
    }

    /// Whether this actor manages accounts: roles, sanctions, passwords. It
    /// takes the role; an override may take it away but never hands it out,
    /// so a curator given the capability still cannot reset a password.
    pub fn manages_accounts(&self) -> bool {
        self.can(Capability::UserRoleManage) && self.can_by_role(Capability::UserRoleManage)
    }

    /// Whether this actor may hand out `target`: only roles below their own, so
    /// nobody can create a peer. Root is exempt.
    pub fn may_grant(&self, target: WikiRole) -> bool {
        if self.global == GlobalRole::Root {
            return true;
        }
        if !self.manages_accounts() {
            return false;
        }
        self.effective_role().is_some_and(|mine| target < mine)
    }
}

/// Builds the actor for one request against one wiki. Anonymous visitors
/// cost no query.
pub async fn resolve(
    db: &sqlx::PgPool,
    wiki_id: Uuid,
    settings: &Value,
    user: Option<&CurrentUser>,
) -> Result<Actor, AppError> {
    let rules = Rules::from_settings(settings);
    let Some(user) = user else {
        return Ok(Actor::anonymous(rules));
    };
    let membership = sqlx::query!(
        r#"SELECT role::text AS role FROM wiki_memberships WHERE user_id = $1 AND wiki_id = $2"#,
        user.id,
        wiki_id
    )
    .fetch_optional(db)
    .await?
    .and_then(|row| row.role)
    .as_deref()
    .and_then(WikiRole::parse);
    let overrides = sqlx::query!(
        "SELECT capability, allowed FROM user_capabilities WHERE user_id = $1 AND wiki_id = $2",
        user.id,
        wiki_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .filter_map(|row| Capability::parse(&row.capability).map(|cap| (cap, row.allowed)))
    .collect();
    // A ban outranks a mute. Install-wide bans are refused by the session layer.
    let sanction = sqlx::query_scalar!(
        "SELECT kind FROM sanctions
         WHERE user_id = $1 AND wiki_id = $2 AND lifted_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())",
        user.id,
        wiki_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|kind| {
        if kind == "ban" {
            Sanction::Ban
        } else {
            Sanction::Mute
        }
    })
    .max_by_key(|s| matches!(s, Sanction::Ban));
    Ok(Actor {
        user_id: Some(user.id),
        username: Some(user.username.clone()),
        display_name: user.display_name.clone(),
        avatar_url: user.avatar_url.clone(),
        email_verified: user.email_verified,
        global: GlobalRole::parse(&user.global_role),
        membership,
        rules,
        overrides,
        sanction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_and_sanctions_sit_on_top_of_the_role() {
        let mut editor = actor(GlobalRole::Registered, None);
        assert!(editor.can(Capability::PageEdit));
        assert!(!editor.can(Capability::PageLock));
        editor.overrides = vec![(Capability::PageLock, true), (Capability::PageEdit, false)];
        assert!(
            editor.can(Capability::PageLock),
            "an allow adds to the role"
        );
        assert!(!editor.can(Capability::PageEdit), "a deny takes from it");
        editor.overrides.clear();
        editor.sanction = Some(Sanction::Mute);
        assert!(!editor.can(Capability::PageEdit));
        assert!(!editor.can(Capability::PageCreate));
        let mut admin = actor(GlobalRole::Registered, Some(WikiRole::Admin));
        admin.sanction = Some(Sanction::Mute);
        assert!(
            admin.can(Capability::AdminPanel),
            "a mute is about publishing"
        );
        admin.sanction = Some(Sanction::Ban);
        assert!(
            !admin.can(Capability::AdminPanel),
            "a ban leaves reading only"
        );
        let mut muted_but_allowed = actor(GlobalRole::Registered, None);
        muted_but_allowed.overrides = vec![(Capability::PageEdit, true)];
        muted_but_allowed.sanction = Some(Sanction::Mute);
        assert!(
            !muted_but_allowed.can(Capability::PageEdit),
            "no override beats a sanction"
        );
    }

    #[test]
    fn capabilities_round_trip_through_their_names() {
        for cap in Capability::ALL {
            assert_eq!(Capability::parse(cap.as_str()), Some(cap));
        }
        assert_eq!(Capability::parse("nope"), None);
    }
    use serde_json::json;

    fn actor(global: GlobalRole, membership: Option<WikiRole>) -> Actor {
        Actor {
            user_id: Some(Uuid::nil()),
            username: Some("tester".into()),
            display_name: None,
            avatar_url: None,
            email_verified: true,
            global,
            membership,
            rules: Rules::default(),
            overrides: Vec::new(),
            sanction: None,
        }
    }

    #[test]
    fn a_guest_can_read_and_nothing_else() {
        let guest = Actor::anonymous(Rules::default());
        assert!(!guest.can(Capability::PageCreate));
        assert!(!guest.can(Capability::PageEdit));
        assert!(!guest.can(Capability::PageDelete));
        assert!(!guest.can(Capability::AdminPanel));
        assert_eq!(guest.effective_role(), None);
    }

    #[test]
    fn a_plain_account_writes_but_does_not_moderate() {
        let user = actor(GlobalRole::Registered, None);
        assert!(user.can(Capability::PageCreate));
        assert!(user.can(Capability::PageEdit));
        assert!(!user.can(Capability::PageDelete));
        assert!(!user.can(Capability::PageLock));
        assert!(!user.can(Capability::AdminPanel));
        assert!(!user.can(Capability::AuditRead));
    }

    #[test]
    fn a_sponsor_is_a_tier_not_a_promotion() {
        let sponsor = actor(GlobalRole::Registered, Some(WikiRole::Sponsor));
        let plain = actor(GlobalRole::Registered, Some(WikiRole::Registered));
        for cap in [
            Capability::PageCreate,
            Capability::PageEdit,
            Capability::PageDelete,
            Capability::PageLock,
            Capability::AdminPanel,
        ] {
            assert_eq!(sponsor.can(cap), plain.can(cap), "{cap:?}");
        }
    }

    #[test]
    fn the_ladder_climbs_in_the_declared_order() {
        let moderator = actor(GlobalRole::Registered, Some(WikiRole::Moderator));
        assert!(moderator.can(Capability::PageDelete));
        assert!(moderator.can(Capability::PageLock));
        assert!(moderator.can(Capability::AuditRead));
        assert!(!moderator.can(Capability::AdminPanel));
        assert!(!moderator.can(Capability::WikiSettings));

        let admin = actor(GlobalRole::Registered, Some(WikiRole::Admin));
        assert!(admin.can(Capability::AdminPanel));
        assert!(admin.can(Capability::WikiSettings));
        assert!(admin.can(Capability::UserRoleManage));
    }

    #[test]
    fn staff_are_moderators_everywhere_and_never_demoted_by_a_membership() {
        let staff = actor(GlobalRole::Staff, None);
        assert_eq!(staff.effective_role(), Some(WikiRole::Moderator));
        assert!(staff.can(Capability::PageDelete));
        assert!(!staff.can(Capability::AdminPanel));

        let pinned = actor(GlobalRole::Staff, Some(WikiRole::Registered));
        assert_eq!(pinned.effective_role(), Some(WikiRole::Moderator));

        let both = actor(GlobalRole::Staff, Some(WikiRole::Admin));
        assert_eq!(both.effective_role(), Some(WikiRole::Admin));
        assert!(both.can(Capability::AdminPanel));
    }

    #[test]
    fn root_holds_every_capability_without_a_membership() {
        let root = actor(GlobalRole::Root, None);
        for cap in [
            Capability::PageCreate,
            Capability::PageEdit,
            Capability::PageDelete,
            Capability::PageLock,
            Capability::RevisionPatrol,
            Capability::AuditRead,
            Capability::AdminPanel,
            Capability::UserRoleManage,
            Capability::WikiSettings,
        ] {
            assert!(root.can(cap), "{cap:?}");
        }
        assert_eq!(root.effective_role(), Some(WikiRole::Owner));
    }

    #[test]
    fn an_unknown_global_role_is_the_least_privileged_reading() {
        assert_eq!(GlobalRole::parse("Root"), GlobalRole::Registered);
        assert_eq!(GlobalRole::parse("superuser"), GlobalRole::Registered);
        assert_eq!(GlobalRole::parse(""), GlobalRole::Registered);
    }

    #[test]
    fn opening_a_wiki_to_anonymous_edits_is_a_setting() {
        let open = Rules::from_settings(&json!({
            "permissions": { "anonymous_edit": true }
        }));
        let guest = Actor::anonymous(open);
        assert!(guest.can(Capability::PageEdit));
        assert!(!guest.can(Capability::PageCreate));
        assert!(!guest.can(Capability::PageDelete));
    }

    #[test]
    fn closing_a_wiki_to_registered_edits_does_not_lock_out_moderators() {
        let closed = Rules::from_settings(&json!({
            "permissions": { "registered_edit": false, "registered_create": false }
        }));
        let mut plain = actor(GlobalRole::Registered, None);
        plain.rules = closed;
        assert!(!plain.can(Capability::PageEdit));

        let mut moderator = actor(GlobalRole::Registered, Some(WikiRole::Moderator));
        moderator.rules = closed;
        assert!(moderator.can(Capability::PageEdit));
        assert!(moderator.can(Capability::PageCreate));
    }

    #[test]
    fn malformed_settings_never_widen_access() {
        for broken in [
            json!({}),
            json!({ "permissions": null }),
            json!({ "permissions": "yes" }),
            json!({ "permissions": [] }),
            json!({ "permissions": { "anonymous_edit": "true" } }),
            json!({ "permissions": { "anonymous_edit": 1 } }),
        ] {
            let rules = Rules::from_settings(&broken);
            assert!(!rules.anonymous_edit, "{broken}");
            assert!(!rules.anonymous_create, "{broken}");
        }
    }

    #[test]
    fn requiring_a_verified_email_blocks_writes_until_it_is_verified() {
        let strict = Rules::from_settings(&json!({
            "permissions": { "require_verified_email": true }
        }));
        let mut unverified = actor(GlobalRole::Registered, None);
        unverified.rules = strict;
        unverified.email_verified = false;
        assert!(!unverified.can(Capability::PageEdit));
        assert!(!unverified.can(Capability::PageCreate));

        let mut verified = unverified.clone();
        verified.email_verified = true;
        assert!(verified.can(Capability::PageEdit));
    }

    #[test]
    fn protection_stops_everyone_below_its_level() {
        let editor = actor(GlobalRole::Registered, None);
        assert!(editor.can_edit_page(None));
        assert!(!editor.can_edit_page(Some(WikiRole::Curator)));

        let curator = actor(GlobalRole::Registered, Some(WikiRole::Curator));
        assert!(curator.can_edit_page(Some(WikiRole::Curator)));
        assert!(!curator.can_edit_page(Some(WikiRole::Moderator)));

        let moderator = actor(GlobalRole::Registered, Some(WikiRole::Moderator));
        assert!(moderator.can_edit_page(Some(WikiRole::Moderator)));

        assert!(!Actor::anonymous(Rules::default()).can_edit_page(None));
    }

    #[test]
    fn protection_goes_up_to_your_own_level_and_no_further() {
        let curator = actor(GlobalRole::Registered, Some(WikiRole::Curator));
        assert!(curator.may_protect(None, Some(WikiRole::Curator)));
        assert!(!curator.may_protect(None, Some(WikiRole::Moderator)));
        assert!(
            !curator.may_protect(Some(WikiRole::Moderator), None),
            "cannot loosen above you"
        );
        let editor = actor(GlobalRole::Registered, None);
        assert!(!editor.may_protect(None, Some(WikiRole::Curator)));
        let admin = actor(GlobalRole::Registered, Some(WikiRole::Admin));
        assert!(admin.may_protect(Some(WikiRole::Moderator), Some(WikiRole::Admin)));
        assert!(!admin.may_protect(None, Some(WikiRole::Owner)));
    }

    #[test]
    fn nobody_grants_a_role_at_or_above_their_own() {
        let admin = actor(GlobalRole::Registered, Some(WikiRole::Admin));
        assert!(admin.may_grant(WikiRole::Registered));
        assert!(admin.may_grant(WikiRole::Sponsor));
        assert!(admin.may_grant(WikiRole::Moderator));
        assert!(!admin.may_grant(WikiRole::Admin));
        assert!(!admin.may_grant(WikiRole::Owner));

        let owner = actor(GlobalRole::Registered, Some(WikiRole::Owner));
        assert!(owner.may_grant(WikiRole::Admin));
        assert!(!owner.may_grant(WikiRole::Owner));

        assert!(actor(GlobalRole::Root, None).may_grant(WikiRole::Owner));

        let moderator = actor(GlobalRole::Registered, Some(WikiRole::Moderator));
        assert!(!moderator.may_grant(WikiRole::Registered));
    }

    #[test]
    fn managing_accounts_takes_the_role_not_an_override() {
        for role in [WikiRole::Curator, WikiRole::Moderator] {
            let mut given = actor(GlobalRole::Registered, Some(role));
            given.overrides = vec![(Capability::UserRoleManage, true)];
            assert!(given.can(Capability::UserRoleManage));
            assert!(!given.manages_accounts(), "{role:?} with an override");
            assert!(!given.may_grant(WikiRole::Registered));
        }
        // Staff moderate everywhere; accounts are not theirs to run.
        assert!(!actor(GlobalRole::Staff, None).manages_accounts());

        let admin = actor(GlobalRole::Registered, Some(WikiRole::Admin));
        assert!(admin.manages_accounts());
        let mut denied = admin.clone();
        denied.overrides = vec![(Capability::UserRoleManage, false)];
        assert!(!denied.manages_accounts());
        let mut banned = admin;
        banned.sanction = Some(Sanction::Ban);
        assert!(!banned.manages_accounts());
    }

    #[test]
    fn role_names_round_trip_through_the_database_spelling() {
        for role in WikiRole::ALL {
            assert_eq!(WikiRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(WikiRole::parse("god"), None);
        for global in [GlobalRole::Registered, GlobalRole::Staff, GlobalRole::Root] {
            assert_eq!(GlobalRole::parse(global.as_str()), global);
        }
    }
}

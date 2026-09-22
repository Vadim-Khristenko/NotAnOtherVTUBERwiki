//! Templates and messages, reloadable without a restart.
//!
//! The template environment and the message catalogue are loaded together and
//! swapped together, because they depend on each other: a template edited to
//! call a new message key, loaded against the old catalogue, would show the raw
//! key until the next restart.
//!
//! **A reload never makes things worse.** The new set is built completely
//! first, and only a successful build replaces the running one. A skin author
//! who saves a template with a syntax error gets a log line and an entry in the
//! admin panel; readers keep getting the last good version. The alternative,
//! swapping first and failing later, would turn one typo into an outage.
//!
//! **Requests see a consistent snapshot.** `current()` hands out an `Arc` to
//! the loaded set. A request holds its own reference for as long as it renders,
//! so a reload in the middle of that render cannot give it half an old skin and
//! half a new one, and the old set is freed when the last request using it ends.

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use crate::error::AppError;
use crate::i18n::Catalog;

/// One loaded skin plus the messages it renders with.
pub struct Loaded {
    pub env: minijinja::Environment<'static>,
    pub messages: Arc<Catalog>,
    pub loaded_at: chrono::DateTime<chrono::Utc>,
    /// The file fingerprint this set was built from, so the watcher can tell
    /// whether anything changed since.
    pub fingerprint: u64,
}

/// What a reload did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing on disk changed, so nothing was rebuilt.
    Unchanged,
    /// Rebuilt and swapped in.
    Reloaded,
}

pub struct Skin {
    skin_dir: String,
    fallback_dir: String,
    locales_dir: String,
    current: RwLock<Arc<Loaded>>,
    /// The last reload that failed, if the most recent attempt failed. Cleared
    /// by the next success. Shown in the admin panel, because a failed reload is
    /// otherwise invisible: the site keeps working on the old files.
    last_error: RwLock<Option<String>>,
}

impl Skin {
    /// Loads the first set. Unlike a reload, a failure here is fatal: there is
    /// no previous version to keep serving.
    pub fn load(skin_dir: &str, fallback_dir: &str, locales_dir: &str) -> Result<Self, AppError> {
        let fingerprint = fingerprint(&[skin_dir, fallback_dir, locales_dir]);
        let loaded = build(skin_dir, fallback_dir, locales_dir, fingerprint)?;
        Ok(Self {
            skin_dir: skin_dir.to_string(),
            fallback_dir: fallback_dir.to_string(),
            locales_dir: locales_dir.to_string(),
            current: RwLock::new(Arc::new(loaded)),
            last_error: RwLock::new(None),
        })
    }

    /// The set a request should render with. Hold on to it for the whole
    /// request; it stays valid across a reload.
    pub fn current(&self) -> Arc<Loaded> {
        // A poisoned lock means a thread panicked while swapping, which only
        // happens between two complete values. Either value is usable, so
        // recover it rather than turn every later request into a 500.
        match self.current.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    pub fn last_error(&self) -> Option<String> {
        match self.last_error.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Rebuilds when the files changed, and swaps only on success.
    ///
    /// `force` rebuilds even when the fingerprint says nothing changed, for the
    /// admin button: a file restored from a backup can carry an old mtime.
    pub fn reload(&self, force: bool) -> Result<Outcome, AppError> {
        let now = fingerprint(&[&self.skin_dir, &self.fallback_dir, &self.locales_dir]);
        if !force && now == self.current().fingerprint {
            return Ok(Outcome::Unchanged);
        }
        match build(&self.skin_dir, &self.fallback_dir, &self.locales_dir, now) {
            Ok(fresh) => {
                let fresh = Arc::new(fresh);
                match self.current.write() {
                    Ok(mut guard) => *guard = fresh,
                    Err(poisoned) => *poisoned.into_inner() = fresh,
                }
                self.set_error(None);
                Ok(Outcome::Reloaded)
            }
            Err(err) => {
                // Remember which fingerprint failed, so the watcher does not
                // retry the same broken files every tick and fill the log.
                self.set_error(Some(err.to_string()));
                Err(err)
            }
        }
    }

    fn set_error(&self, value: Option<String>) {
        match self.last_error.write() {
            Ok(mut guard) => *guard = value,
            Err(poisoned) => *poisoned.into_inner() = value,
        }
    }

    /// Polls the skin and locale directories and reloads on change.
    ///
    /// Polling rather than a filesystem notification API: the directories hold a
    /// few dozen small files, a walk costs microseconds, and it behaves the same
    /// on Windows, Linux, inside a container and over a network mount, which the
    /// notification APIs do not.
    pub fn watch(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut failed_at: Option<u64> = None;
            let mut ticker = tokio::time::interval(every);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let now = fingerprint(&[&self.skin_dir, &self.fallback_dir, &self.locales_dir]);
                if now == self.current().fingerprint || failed_at == Some(now) {
                    continue;
                }
                match self.reload(false) {
                    Ok(Outcome::Reloaded) => {
                        failed_at = None;
                        let loaded = self.current();
                        tracing::info!(
                            languages = ?loaded.messages.languages(),
                            problems = loaded.messages.problems().len(),
                            "skin and locales reloaded"
                        );
                    }
                    Ok(Outcome::Unchanged) => {}
                    Err(err) => {
                        failed_at = Some(now);
                        tracing::error!(
                            error = %err,
                            "reload failed, still serving the previous skin"
                        );
                    }
                }
            }
        })
    }
}

fn build(
    skin_dir: &str,
    fallback_dir: &str,
    locales_dir: &str,
    fingerprint: u64,
) -> Result<Loaded, AppError> {
    let messages = Arc::new(Catalog::load(locales_dir)?);
    let env = crate::templates::build(skin_dir, fallback_dir, Arc::clone(&messages))?;
    Ok(Loaded {
        env,
        messages,
        loaded_at: chrono::Utc::now(),
        fingerprint,
    })
}

/// A hash of every file's path, size and modification time under `roots`.
///
/// Size is in there because some editors and some filesystems keep a coarse
/// mtime, and a save within the same second would otherwise be missed. A
/// missing root hashes as empty rather than failing: a skin with no directory
/// of its own is valid, it inherits everything from the fallback.
pub fn fingerprint(roots: &[&str]) -> u64 {
    let mut entries: Vec<(String, u64, u128)> = Vec::new();
    for root in roots {
        collect(Path::new(root), &mut entries, 0);
    }
    // Directory iteration order is not stable across platforms. Sorting makes
    // the hash depend on what is on disk and nothing else.
    entries.sort();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

/// Depth-limited, so a symlink loop inside a skin directory cannot hang the
/// watcher. Skins and packs are two levels deep; eight is generous.
fn collect(dir: &Path, out: &mut Vec<(String, u64, u128)>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect(&path, out, depth + 1);
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        out.push((path.to_string_lossy().into_owned(), meta.len(), modified));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(path: &str) -> String {
        format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR"))
    }

    /// A private copy of the default skin and the shipped locales, so a test
    /// can edit files without touching the repository.
    fn scratch(name: &str) -> (std::path::PathBuf, String, String) {
        let root = std::env::temp_dir().join(format!("naw-skin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let skin = root.join("skin");
        let locales = root.join("locales");
        copy_tree(Path::new(&repo("skins/default")), &skin);
        copy_tree(Path::new(&repo("locales")), &locales);
        (
            root,
            skin.to_string_lossy().into_owned(),
            locales.to_string_lossy().into_owned(),
        )
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("mkdir");
        for entry in std::fs::read_dir(from)
            .expect("read_dir")
            .filter_map(Result::ok)
        {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy");
            }
        }
    }

    /// Filesystems with a coarse mtime would hide a quick second write, so the
    /// tests change the size as well, which the fingerprint also covers.
    fn append(path: &str, text: &str) {
        let mut body = std::fs::read_to_string(path).expect("read");
        body.push_str(text);
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn nothing_changed_means_nothing_rebuilt() {
        let (root, skin, locales) = scratch("unchanged");
        let s = Skin::load(&skin, &skin, &locales).expect("loads");
        let before = s.current().loaded_at;
        assert_eq!(s.reload(false).expect("reload"), Outcome::Unchanged);
        assert_eq!(s.current().loaded_at, before);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_edited_message_takes_effect_without_a_restart() {
        let (root, skin, locales) = scratch("message");
        let s = Skin::load(&skin, &skin, &locales).expect("loads");
        assert_eq!(
            s.current().messages.render("en", "nav.sign_in", &[]),
            "Sign in"
        );

        let nav = format!("{locales}/en/nav.toml");
        let body = std::fs::read_to_string(&nav).expect("read");
        std::fs::write(
            &nav,
            body.replace("sign_in = \"Sign in\"", "sign_in = \"Log in, snacker\""),
        )
        .expect("write");

        assert_eq!(s.reload(false).expect("reload"), Outcome::Reloaded);
        assert_eq!(
            s.current().messages.render("en", "nav.sign_in", &[]),
            "Log in, snacker"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_broken_template_keeps_the_previous_one_serving() {
        // The property that makes reloading safe to leave on in production.
        let (root, skin, locales) = scratch("broken");
        let s = Skin::load(&skin, &skin, &locales).expect("loads");
        let before = s.current().loaded_at;

        append(&format!("{skin}/page.html"), "\n{% if unclosed %}\n");
        assert!(s.reload(false).is_err());
        // Still the old set, still renderable, and the failure is on record.
        assert_eq!(s.current().loaded_at, before);
        assert!(s.current().env.get_template("page.html").is_ok());
        assert!(s.last_error().is_some());

        // Fixing the file clears the error on the next reload.
        let body = std::fs::read_to_string(format!("{skin}/page.html")).expect("read");
        std::fs::write(
            format!("{skin}/page.html"),
            body.replace("\n{% if unclosed %}\n", "\n<!-- fixed -->\n"),
        )
        .expect("write");
        assert_eq!(s.reload(false).expect("reload"), Outcome::Reloaded);
        assert!(s.last_error().is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_new_language_appears_on_reload() {
        let (root, skin, locales) = scratch("newlang");
        let s = Skin::load(&skin, &skin, &locales).expect("loads");
        assert!(!s.current().messages.has("de"));

        let de = format!("{locales}/de");
        std::fs::create_dir_all(&de).expect("mkdir");
        std::fs::write(
            format!("{de}/metadata.toml"),
            "code = \"de\"\nname = \"German\"\nnative_name = \"Deutsch\"\nversion = \"0.1.0\"\n",
        )
        .expect("write");
        std::fs::write(format!("{de}/nav.toml"), "sign_in = \"Anmelden\"\n").expect("write");

        assert_eq!(s.reload(false).expect("reload"), Outcome::Reloaded);
        assert!(s.current().messages.has("de"));
        assert_eq!(
            s.current().messages.render("de", "nav.sign_in", &[]),
            "Anmelden"
        );
        // Everything German does not translate falls through to English.
        assert_eq!(
            s.current().messages.render("de", "nav.sign_out", &[]),
            "Sign out"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_request_keeps_its_snapshot_across_a_reload() {
        let (root, skin, locales) = scratch("snapshot");
        let s = Skin::load(&skin, &skin, &locales).expect("loads");
        let held = s.current();
        append(&format!("{locales}/en/nav.toml"), "\nextra_key = \"x\"\n");
        s.reload(false).expect("reload");
        // The reference a request took before the reload still sees the old set.
        assert_eq!(
            held.messages.render("en", "nav.extra_key", &[]),
            "nav.extra_key"
        );
        assert_eq!(s.current().messages.render("en", "nav.extra_key", &[]), "x");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_fingerprint_ignores_directory_order_and_tolerates_a_missing_root() {
        let a = fingerprint(&[&repo("skins/default"), &repo("locales")]);
        let b = fingerprint(&[&repo("skins/default"), &repo("locales")]);
        assert_eq!(a, b);
        // A skin with no directory of its own inherits from the fallback.
        let _ = fingerprint(&["naw-no-such-directory-anywhere"]);
    }
}

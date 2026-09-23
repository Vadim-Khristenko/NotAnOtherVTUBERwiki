//! File-backed MiniJinja templates with per-file fallback to the default skin.

use std::sync::Arc;

use minijinja::Environment;

use crate::error::AppError;
use crate::i18n::Catalog;

const TEMPLATES: &[&str] = &[
    "layout.html",
    // Colors on top of the default styles; a recolor-only skin ships just this.
    "_theme.html",
    "_header.html",
    "_footer.html",
    // Inlined into pages, so a skin can ship its own icon set.
    "icons.svg",
    "page.html",
    "edit.html",
    "login.html",
    "password.html",
    "settings.html",
    "profile.html",
    "untranslated.html",
    "media.html",
    "emotes.html",
    "relay.html",
    "message.html",
    "search.html",
    "history.html",
    "revision.html",
    "diff.html",
    "admin.html",
];

/// Fills templates a custom skin does not ship.
pub const DEFAULT_SKIN_DIR: &str = "skins/default";

const DEFAULT_LOCALES_DIR: &str = "locales";

pub fn load_templates(dir: &str) -> Result<Environment<'static>, AppError> {
    load_templates_with_fallback(dir, DEFAULT_SKIN_DIR, DEFAULT_LOCALES_DIR)
}

/// Loads every template from `dir`, falling back to `fallback_dir` per file,
/// with the translation functions from `locales_dir` installed.
pub fn load_templates_with_fallback(
    dir: &str,
    fallback_dir: &str,
    locales_dir: &str,
) -> Result<Environment<'static>, AppError> {
    build(dir, fallback_dir, Arc::new(Catalog::load(locales_dir)?))
}

/// Builds an environment against an already loaded catalogue, shared with
/// the handlers so templates and Rust agree on the messages.
pub fn build(
    dir: &str,
    fallback_dir: &str,
    catalog: Arc<Catalog>,
) -> Result<Environment<'static>, AppError> {
    let mut env = Environment::new();
    crate::i18n::install(&mut env, catalog);
    // A function rather than a context value, so every template gets it.
    env.add_function("csp_nonce", crate::csp::current);
    for name in TEMPLATES {
        let path = format!("{dir}/{name}");
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(err) if dir != fallback_dir => {
                let fallback = format!("{fallback_dir}/{name}");
                // Inheriting a template is the normal case, so this is not a warning.
                tracing::debug!(
                    template = name,
                    skin = dir,
                    "inherited from the default skin"
                );
                std::fs::read_to_string(&fallback)
                    .map_err(|_| AppError::Config(format!("template {path}: {err}")))?
            }
            Err(err) => return Err(AppError::Config(format!("template {path}: {err}"))),
        };
        env.add_template_owned(name.to_string(), source)
            .map_err(|err| AppError::Config(format!("template {path}: {err}")))?;
    }
    load_error_pages(&mut env, dir, fallback_dir)?;
    Ok(env)
}

/// Loads `errors/*.html` from the default skin, then the active one on top.
/// Scanned rather than listed, so a skin can add any error kind.
fn load_error_pages(
    env: &mut Environment<'static>,
    dir: &str,
    fallback_dir: &str,
) -> Result<(), AppError> {
    let mut found: std::collections::BTreeMap<String, String> = Default::default();
    for root in [fallback_dir, dir] {
        let errors_dir = format!("{root}/errors");
        let Ok(entries) = std::fs::read_dir(&errors_dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "html") {
                continue;
            }
            let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            found.insert(
                format!("errors/{file}"),
                path.to_string_lossy().into_owned(),
            );
        }
    }
    if !found.contains_key("errors/_generic.html") {
        return Err(AppError::Config(format!(
            "no errors/_generic.html in {dir} or {fallback_dir}: every error page falls back to it"
        )));
    }
    for (name, path) in found {
        let source = std::fs::read_to_string(&path)
            .map_err(|err| AppError::Config(format!("template {path}: {err}")))?;
        env.add_template_owned(name, source)
            .map_err(|err| AppError::Config(format!("template {path}: {err}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_skin() -> String {
        format!("{}/../../skins/default", env!("CARGO_MANIFEST_DIR"))
    }

    fn locales() -> String {
        format!("{}/../../locales", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn missing_templates_fall_back_to_default_skin() {
        let empty = std::env::temp_dir().join("naw-fallback-probe");
        std::fs::create_dir_all(&empty).expect("temp dir");
        let env = load_templates_with_fallback(
            empty.to_str().expect("utf8"),
            &default_skin(),
            &locales(),
        )
        .expect("fallback covers every template");
        assert!(env.get_template("edit.html").is_ok());
        assert!(env.get_template("page.html").is_ok());
        std::fs::remove_dir(&empty).ok();
    }

    #[test]
    fn a_theme_only_skin_inherits_the_rest_and_lands_in_the_head() {
        let snackers = format!("{}/../../skins/snackers", env!("CARGO_MANIFEST_DIR"));
        let env = load_templates_with_fallback(&snackers, &default_skin(), &locales())
            .expect("theme only skin loads");
        let html = env
            .get_template("message.html")
            .expect("inherited")
            .render(minijinja::context! { title => "t", wiki_name => "w", lang => "en" })
            .expect("renders");
        let head = &html[..html.find("</head>").expect("has a head")];
        assert!(
            head.contains("--accent: #ff3d8b"),
            "theme missing from head"
        );
        assert!(
            head.rfind("--accent: #ff3d8b") > head.find("--accent: #0b5fff"),
            "the theme must come after the default styles to win"
        );
    }

    #[test]
    fn missing_everywhere_is_still_an_error() {
        let err =
            load_templates_with_fallback("naw-no-such-dir-a", "naw-no-such-dir-b", &locales())
                .expect_err("must fail");
        assert!(matches!(err, AppError::Config(_)));
    }

    #[test]
    fn the_translation_functions_are_available_to_every_template() {
        let env = load_templates_with_fallback(&default_skin(), &default_skin(), &locales())
            .expect("default skin loads");
        let rendered = env
            .render_str(
                "{{ t('nav.search') }}|{{ t('definitely.not.a.key') }}|{{ tn('x.y', 2) }}",
                minijinja::context! { lang => "en" },
            )
            .expect("t and tn are registered");
        let parts: Vec<&str> = rendered.split('|').collect();
        assert!(!parts[0].is_empty());
        assert_eq!(parts[1], "definitely.not.a.key");
        assert_eq!(parts[2], "x.y");
    }
}

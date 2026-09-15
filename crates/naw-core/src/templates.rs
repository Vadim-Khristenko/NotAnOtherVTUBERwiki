//! File-backed MiniJinja templates for the default skin.

use minijinja::Environment;

use crate::error::AppError;

const TEMPLATES: &[&str] = &[
    "layout.html",
    "_header.html",
    "_footer.html",
    "page.html",
    "404.html",
    "edit.html",
];

/// Reference skin that fills templates a custom skin does not ship.
const DEFAULT_SKIN_DIR: &str = "skins/default";

pub fn load_templates(dir: &str) -> Result<Environment<'static>, AppError> {
    load_templates_with_fallback(dir, DEFAULT_SKIN_DIR)
}

/// Loads every template from `dir`, falling back to `fallback_dir` per file.
/// A skin ships only what it restyles; the reference skin fills the rest, so
/// a skin without `edit.html` renders the editor instead of failing.
pub fn load_templates_with_fallback(
    dir: &str,
    fallback_dir: &str,
) -> Result<Environment<'static>, AppError> {
    let mut env = Environment::new();
    for name in TEMPLATES {
        let path = format!("{dir}/{name}");
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(err) if dir != fallback_dir => {
                let fallback = format!("{fallback_dir}/{name}");
                tracing::warn!(
                    template = name,
                    skin = dir,
                    "template missing, using default"
                );
                std::fs::read_to_string(&fallback)
                    .map_err(|_| AppError::Config(format!("template {path}: {err}")))?
            }
            Err(err) => return Err(AppError::Config(format!("template {path}: {err}"))),
        };
        env.add_template_owned(name.to_string(), source)
            .map_err(|err| AppError::Config(format!("template {path}: {err}")))?;
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_skin() -> String {
        format!("{}/../../skins/default", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn missing_templates_fall_back_to_default_skin() {
        let empty = std::env::temp_dir().join("naw-fallback-probe");
        std::fs::create_dir_all(&empty).expect("temp dir");
        let env = load_templates_with_fallback(empty.to_str().expect("utf8"), &default_skin())
            .expect("fallback covers every template");
        assert!(env.get_template("edit.html").is_ok());
        assert!(env.get_template("page.html").is_ok());
        std::fs::remove_dir(&empty).ok();
    }

    #[test]
    fn missing_everywhere_is_still_an_error() {
        let err = load_templates_with_fallback("naw-no-such-dir-a", "naw-no-such-dir-b")
            .expect_err("must fail");
        assert!(matches!(err, AppError::Config(_)));
    }
}

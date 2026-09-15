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

pub fn load_templates(dir: &str) -> Result<Environment<'static>, AppError> {
    let mut env = Environment::new();
    for name in TEMPLATES {
        let path = format!("{dir}/{name}");
        let source = std::fs::read_to_string(&path)
            .map_err(|err| AppError::Config(format!("template {path}: {err}")))?;
        env.add_template_owned(name.to_string(), source)
            .map_err(|err| AppError::Config(format!("template {path}: {err}")))?;
    }
    Ok(env)
}

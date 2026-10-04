//! A skin's stylesheets, scripts and fonts, served with a content hash.
//!
//! A skin may ship `styles/`, `scripts/` and `fonts/` next to its templates.
//! They load with the templates and reload with them; a file the active
//! skin does not have comes from the default skin, as templates do. Each
//! file gets a short SHA-256, so a page can link `?v=<hash>` and the file
//! can be cached for a year: a new version is a new address.
//!
//! Text files are gzipped once at load, so a script reaches the browser
//! compressed even where no proxy in front compresses it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use axum::body::Bytes;

use sha2::{Digest, Sha256};

/// The folders a skin may ship assets in.
pub const FOLDERS: [&str; 3] = ["styles", "scripts", "fonts"];

/// Largest single asset, and all of a skin's assets together.
const FILE_MAX: u64 = 5 * 1024 * 1024;
const TOTAL_MAX: u64 = 40 * 1024 * 1024;

/// One asset, ready to serve.
#[derive(Debug)]
pub struct Asset {
    pub bytes: Bytes,
    /// The same bytes gzipped, when that is smaller.
    pub gzip: Option<Bytes>,
    /// The first 12 hex characters of its SHA-256.
    pub hash: String,
    pub content_type: &'static str,
}

/// Every asset of a skin, by path under the skin: `scripts/editor.js`.
#[derive(Debug, Default)]
pub struct Assets {
    files: BTreeMap<String, Asset>,
}

impl Assets {
    pub fn get(&self, path: &str) -> Option<&Asset> {
        self.files.get(path)
    }

    /// The address a page links: `/skin/a/scripts/editor.js?v=1a2b3c4d5e6f`,
    /// or `None` when no skin has the file.
    pub fn url(&self, path: &str) -> Option<String> {
        self.files
            .get(path)
            .map(|asset| format!("/skin/a/{path}?v={}", asset.hash))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Loads the default skin's assets, then the active skin's over them.
    pub fn load(skin_dir: &str, fallback_dir: &str) -> Self {
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        let roots: Vec<&str> = if skin_dir == fallback_dir {
            vec![skin_dir]
        } else {
            vec![fallback_dir, skin_dir]
        };
        for root in roots {
            for folder in FOLDERS {
                let dir = Path::new(root).join(folder);
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.filter_map(Result::ok) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let Some(content_type) = content_type(&name) else {
                        continue;
                    };
                    if !name_is_plain(&name) {
                        tracing::warn!(file = %name, "skin asset with an unusual name skipped");
                        continue;
                    }
                    let Ok(meta) = entry.metadata() else {
                        continue;
                    };
                    if !meta.is_file() || meta.len() > FILE_MAX || total + meta.len() > TOTAL_MAX {
                        tracing::warn!(file = %name, "skin asset too large, skipped");
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(entry.path()) else {
                        continue;
                    };
                    total += bytes.len() as u64;
                    let hash = hex::encode(Sha256::digest(&bytes))[..12].to_string();
                    let gzip = compressible(content_type).then(|| gzip(&bytes)).flatten();
                    files.insert(
                        format!("{folder}/{name}"),
                        Asset {
                            bytes: Bytes::from(bytes),
                            gzip,
                            hash,
                            content_type,
                        },
                    );
                }
            }
        }
        Self { files }
    }
}

/// Letters, digits, dots, dashes and underscores, and no leading dot.
fn name_is_plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn content_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

fn compressible(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || content_type == "application/json"
        || content_type == "image/svg+xml"
}

fn gzip(bytes: &[u8]) -> Option<Bytes> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).ok()?;
    let packed = encoder.finish().ok()?;
    (packed.len() < bytes.len()).then(|| Bytes::from(packed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_skin(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("naw-assets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scripts")).expect("dir");
        std::fs::create_dir_all(dir.join("styles")).expect("dir");
        dir
    }

    #[test]
    fn the_active_skin_wins_and_the_default_fills_in() {
        let base = temp_skin("base");
        let mine = temp_skin("mine");
        std::fs::write(
            base.join("scripts/editor.js"),
            "console.log('base');".repeat(50),
        )
        .expect("w");
        std::fs::write(base.join("styles/editor.css"), "a{}").expect("w");
        std::fs::write(mine.join("styles/editor.css"), "b{color:red}").expect("w");
        std::fs::write(mine.join("styles/.hidden.css"), "x").expect("w");
        std::fs::write(mine.join("styles/notes.txt"), "x").expect("w");
        let assets = Assets::load(mine.to_str().unwrap(), base.to_str().unwrap());
        assert_eq!(assets.len(), 2, "only plain names of known kinds");
        assert_eq!(
            assets.get("styles/editor.css").map(|a| a.bytes.as_ref()),
            Some(&b"b{color:red}"[..])
        );
        let js = assets.get("scripts/editor.js").expect("inherited");
        assert!(
            js.gzip.as_ref().is_some_and(|g| g.len() < js.bytes.len()),
            "compressed once"
        );
        let url = assets.url("scripts/editor.js").expect("url");
        assert!(
            url.starts_with("/skin/a/scripts/editor.js?v=")
                && url.len() == "/skin/a/scripts/editor.js?v=".len() + 12
        );
        assert!(assets.url("scripts/nope.js").is_none());
    }
}

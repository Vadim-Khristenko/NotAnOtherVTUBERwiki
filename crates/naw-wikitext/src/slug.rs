//! Page and file names as the engine spells them.

/// Longest page address the importer makes, in characters.
const SLUG_MAX: usize = 120;

/// The engine's address for a MediaWiki title: `Bao the Whale` is
/// `bao-the-whale`, `Neuro-sama/Evil` is `neuro-sama-evil`. Latin letters
/// lose their accents, Cyrillic is transliterated, apostrophes vanish
/// (`Filian's` is `filians`) and anything else separates words. A title
/// with nothing left, as one only in Japanese, gets a name made from its
/// hash, the same every time, so links to it still find it. `None` only for
/// an empty title.
pub fn page_slug(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    Some(spelled(title).unwrap_or_else(|| format!("p-{:08x}", fnv1a(title))))
}

/// The title in Latin letters, or `None` when nothing is left.
fn spelled(title: &str) -> Option<String> {
    let mut out = String::with_capacity(title.len());
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            continue;
        }
        if matches!(c, '\'' | '\u{2019}' | '\u{2018}') {
            continue;
        }
        let lower = c.to_lowercase().next().unwrap_or(c);
        match translit(lower) {
            Some(t) => out.push_str(t),
            None => {
                if !out.is_empty() && !out.ends_with('-') {
                    out.push('-');
                }
            }
        }
    }
    if out.len() > SLUG_MAX {
        out.truncate(SLUG_MAX);
    }
    let out = out.trim_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

/// A file's page in the engine, from its MediaWiki name: `Bao 2.0 New.jpg`
/// is `("image", "bao-2-0-new.jpg")`.
pub fn file_name(name: &str) -> Option<(&'static str, String)> {
    let (raw_stem, ext) = name.trim().rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    if ext.is_empty() || ext.len() > 5 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let mut stem = page_slug(raw_stem)?;
    if spelled(raw_stem).is_none() {
        stem.replace_range(..1, "f");
    }
    stem.truncate(100 - ext.len() - 1);
    let stem = stem.trim_end_matches('-').to_string();
    let prefix = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" | "bmp" => "image",
        "ogg" | "oga" | "mp3" | "wav" | "flac" | "m4a" | "opus" => "audio",
        "webm" | "mp4" | "ogv" | "mov" => "video",
        _ => "file",
    };
    Some((prefix, format!("{stem}.{ext}")))
}

/// FNV-1a, 32 bits: short, stable across versions and platforms.
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in s.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn translit(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ą' | 'ă' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ı' => "i",
        'ł' | 'ľ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'œ' => "oe",
        'ř' => "r",
        'ś' | 'š' | 'ş' => "s",
        'ß' => "ss",
        'ť' | 'ţ' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' | 'ґ' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' => "e",
        'є' => "ye",
        'ж' => "zh",
        'з' => "z",
        'и' | 'і' => "i",
        'ї' => "yi",
        'й' | 'ы' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ъ' | 'ь' => "",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_addresses() {
        assert_eq!(page_slug("Bao the Whale").as_deref(), Some("bao-the-whale"));
        assert_eq!(
            page_slug("Neuro-sama/Evil").as_deref(),
            Some("neuro-sama-evil")
        );
        assert_eq!(page_slug("Filian's Lore").as_deref(), Some("filians-lore"));
        assert_eq!(
            page_slug("  Café_Société ").as_deref(),
            Some("cafe-societe")
        );
        assert_eq!(page_slug("Филиан").as_deref(), Some("filian"));
        assert_eq!(page_slug("VShojo (2020)").as_deref(), Some("vshojo-2020"));
        let japanese = page_slug("バオ").expect("a name from the hash");
        assert!(
            japanese.starts_with("p-") && japanese.len() == 10,
            "{japanese}"
        );
        assert_eq!(page_slug("バオ"), Some(japanese));
        assert_eq!(page_slug("  "), None);
    }

    #[test]
    fn files_keep_their_extension_and_get_a_prefix() {
        assert_eq!(
            file_name("Bao 2.0 New.jpg"),
            Some(("image", "bao-2-0-new.jpg".to_string()))
        );
        assert_eq!(
            file_name("Theme.OGG"),
            Some(("audio", "theme.ogg".to_string()))
        );
        assert_eq!(
            file_name("Notes.pdf"),
            Some(("file", "notes.pdf".to_string()))
        );
        assert_eq!(file_name("no extension"), None);
        let (prefix, name) = file_name("バオ.png").expect("a name from the hash");
        assert_eq!(prefix, "image");
        assert!(name.starts_with("f-") && name.ends_with(".png"), "{name}");
    }
}

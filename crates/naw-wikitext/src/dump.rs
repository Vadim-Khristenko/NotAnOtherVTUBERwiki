//! MediaWiki XML dumps, read one page at a time.
//!
//! The format is what `Special:Export`, `api.php?action=query&export` and
//! FANDOM's database dumps write: `<siteinfo>` with the wiki's namespaces,
//! then a `<page>` per title with its `<revision>`s, oldest first. A full
//! history dump can be gigabytes, so pages stream and only one is in memory.

use std::io::BufRead;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// The wiki the dump came from.
#[derive(Debug, Default, Clone)]
pub struct SiteInfo {
    pub name: String,
    /// The address pages are under, `https://x.fandom.com/wiki/`.
    pub base: String,
    /// Namespace numbers and their local names: 14 is `Category`, or
    /// `Категория` on a Russian wiki.
    pub namespaces: Vec<(i32, String)>,
}

impl SiteInfo {
    /// The local name of namespace `ns`.
    pub fn namespace(&self, ns: i32) -> Option<&str> {
        self.namespaces
            .iter()
            .find(|(n, _)| *n == ns)
            .map(|(_, name)| name.as_str())
    }
}

/// One page with the revisions the dump holds for it.
#[derive(Debug, Default, Clone)]
pub struct Page {
    pub title: String,
    pub ns: i32,
    pub id: u64,
    /// The title a redirect leads to.
    pub redirect: Option<String>,
    pub revisions: Vec<Revision>,
}

impl Page {
    /// The title without its namespace: `Template:Infobox` is `Infobox`.
    pub fn bare_title(&self) -> &str {
        match self.ns {
            0 => &self.title,
            _ => self
                .title
                .split_once(':')
                .map_or(self.title.as_str(), |(_, rest)| rest),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Revision {
    pub id: u64,
    /// ISO 8601 in UTC, `2026-10-08T10:48:47Z`.
    pub timestamp: String,
    pub author: Author,
    pub comment: Option<String>,
    pub minor: bool,
    /// `wikitext` for articles; `css`, `javascript`, `json` or `Scribunto`
    /// (Lua) for code pages, which do not convert.
    pub model: String,
    pub text: String,
}

/// Who made a revision, as the dump names them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Author {
    User(String),
    /// An edit without an account, by address.
    Ip(String),
    /// Removed from public view on the source wiki.
    #[default]
    Hidden,
}

#[derive(Debug)]
pub struct DumpError(pub String);

impl std::fmt::Display for DumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DumpError {}

impl From<quick_xml::Error> for DumpError {
    fn from(err: quick_xml::Error) -> Self {
        DumpError(err.to_string())
    }
}

/// A dump being read.
pub struct Dump<R: BufRead> {
    reader: Reader<R>,
    buf: Vec<u8>,
    /// Filled while the first page is read, since it comes before it.
    pub site: SiteInfo,
    done: bool,
}

impl<R: BufRead> Dump<R> {
    pub fn new(input: R) -> Self {
        let mut reader = Reader::from_reader(input);
        reader.config_mut().trim_text(false);
        Self {
            reader,
            buf: Vec::new(),
            site: SiteInfo::default(),
            done: false,
        }
    }

    /// The next page, or `None` at the end.
    pub fn next_page(&mut self) -> Result<Option<Page>, DumpError> {
        if self.done {
            return Ok(None);
        }
        let mut stack: Vec<String> = Vec::new();
        let mut text = String::new();
        let mut page: Option<Page> = None;
        let mut revision: Option<Revision> = None;
        let mut namespace_key: Option<i32> = None;
        loop {
            self.buf.clear();
            let event = self.reader.read_event_into(&mut self.buf)?;
            match event {
                Event::Eof => {
                    self.done = true;
                    return match page {
                        Some(_) => Err(DumpError("the dump ends inside a page".into())),
                        None => Ok(None),
                    };
                }
                Event::Start(ref e) => {
                    let name = local(e);
                    text.clear();
                    match name.as_str() {
                        "page" => page = Some(Page::default()),
                        "revision" if page.is_some() => revision = Some(Revision::default()),
                        "namespace" => namespace_key = attr(e, "key").and_then(|k| k.parse().ok()),
                        "contributor" => {
                            if let Some(r) = revision.as_mut()
                                && attr(e, "deleted").is_some()
                            {
                                r.author = Author::Hidden;
                            }
                        }
                        _ => {}
                    }
                    stack.push(name);
                }
                Event::Empty(ref e) => {
                    let name = local(e);
                    match name.as_str() {
                        "redirect" => {
                            if let Some(p) = page.as_mut() {
                                p.redirect = attr(e, "title");
                            }
                        }
                        "minor" => {
                            if let Some(r) = revision.as_mut() {
                                r.minor = true;
                            }
                        }
                        "namespace" => {
                            if let Some(key) = attr(e, "key").and_then(|k| k.parse().ok()) {
                                self.site.namespaces.push((key, String::new()));
                            }
                        }
                        _ => {}
                    }
                }
                Event::Text(ref e) => {
                    text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
                Event::CData(ref e) => {
                    text.push_str(&e.xml_content(quick_xml::XmlVersion::Implicit1_0));
                }
                Event::GeneralRef(ref e) => {
                    let resolved = match e.resolve_char_ref() {
                        Ok(Some(c)) => Some(c),
                        _ => match AsRef::<str>::as_ref(&**e) {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        },
                    };
                    if let Some(c) = resolved {
                        text.push(c);
                    }
                }
                Event::End(_) => {
                    let name = stack.pop().unwrap_or_default();
                    let parent = stack.last().map(String::as_str).unwrap_or("");
                    let value = std::mem::take(&mut text);
                    match (parent, name.as_str()) {
                        ("siteinfo", "sitename") => self.site.name = value,
                        ("siteinfo", "base") => self.site.base = value,
                        ("namespaces", "namespace") => {
                            if let Some(key) = namespace_key.take() {
                                self.site.namespaces.push((key, value));
                            }
                        }
                        ("page", "title") => set(&mut page, |p| p.title = value),
                        ("page", "ns") => {
                            set(&mut page, |p| p.ns = value.trim().parse().unwrap_or(0))
                        }
                        ("page", "id") => {
                            set(&mut page, |p| p.id = value.trim().parse().unwrap_or(0))
                        }
                        ("revision", "id") => {
                            set(&mut revision, |r| r.id = value.trim().parse().unwrap_or(0))
                        }
                        ("revision", "timestamp") => set(&mut revision, |r| r.timestamp = value),
                        ("revision", "comment") => set(&mut revision, |r| r.comment = Some(value)),
                        ("revision", "model") => set(&mut revision, |r| r.model = value),
                        ("revision", "text") => set(&mut revision, |r| r.text = value),
                        ("contributor", "username") => {
                            set(&mut revision, |r| r.author = Author::User(value))
                        }
                        ("contributor", "ip") => {
                            set(&mut revision, |r| r.author = Author::Ip(value))
                        }
                        (_, "revision") => {
                            if let (Some(p), Some(r)) = (page.as_mut(), revision.take()) {
                                p.revisions.push(r);
                            }
                        }
                        (_, "page") => {
                            if let Some(mut p) = page.take() {
                                // Exports list revisions oldest first, but
                                // a merged dump may not.
                                p.revisions.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                                return Ok(Some(p));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn set<T>(slot: &mut Option<T>, f: impl FnOnce(&mut T)) {
    if let Some(value) = slot.as_mut() {
        f(value);
    }
}

fn local(e: &BytesStart) -> String {
    AsRef::<str>::as_ref(&e.local_name()).to_string()
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.try_get_attribute(name).ok().flatten().and_then(|a| {
        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()
            .map(|v| v.into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<mediawiki xmlns="http://www.mediawiki.org/xml/export-0.11/" version="0.11">
  <siteinfo>
    <sitename>Test Wiki</sitename>
    <base>https://test.fandom.com/wiki/Main_Page</base>
    <namespaces>
      <namespace key="0" case="first-letter" />
      <namespace key="10" case="first-letter">Template</namespace>
      <namespace key="14" case="first-letter">Category</namespace>
    </namespaces>
  </siteinfo>
  <page>
    <title>Bao the Whale</title>
    <ns>0</ns>
    <id>13698</id>
    <revision>
      <id>2</id>
      <timestamp>2026-10-08T10:48:47Z</timestamp>
      <contributor><username>Second</username><id>9</id></contributor>
      <minor />
      <comment>/* References */ fix</comment>
      <model>wikitext</model>
      <text bytes="20" xml:space="preserve">'''Bao''' &lt;ref&gt;x&lt;/ref&gt; &amp; more</text>
    </revision>
    <revision>
      <id>1</id>
      <timestamp>2020-01-01T00:00:00Z</timestamp>
      <contributor><ip>192.0.2.1</ip></contributor>
      <model>wikitext</model>
      <text xml:space="preserve">first</text>
    </revision>
  </page>
  <page>
    <title>Bao</title>
    <ns>0</ns>
    <id>2</id>
    <redirect title="Bao the Whale" />
    <revision>
      <id>3</id>
      <timestamp>2021-01-01T00:00:00Z</timestamp>
      <contributor deleted="deleted" />
      <model>wikitext</model>
      <text xml:space="preserve">#REDIRECT [[Bao the Whale]]</text>
    </revision>
  </page>
</mediawiki>"#;

    #[test]
    fn pages_stream_with_their_history_oldest_first() {
        let mut dump = Dump::new(SAMPLE.as_bytes());
        let page = dump.next_page().unwrap().expect("first page");
        assert_eq!(dump.site.name, "Test Wiki");
        assert_eq!(dump.site.namespace(14), Some("Category"));
        assert_eq!(page.title, "Bao the Whale");
        assert_eq!((page.ns, page.id), (0, 13698));
        assert_eq!(page.revisions.len(), 2);
        let (first, last) = (&page.revisions[0], &page.revisions[1]);
        assert_eq!(first.author, Author::Ip("192.0.2.1".into()));
        assert_eq!(first.text, "first");
        assert_eq!(last.author, Author::User("Second".into()));
        assert!(last.minor && !first.minor);
        assert_eq!(last.comment.as_deref(), Some("/* References */ fix"));
        assert_eq!(last.text, "'''Bao''' <ref>x</ref> & more");

        let redirect = dump.next_page().unwrap().expect("second page");
        assert_eq!(redirect.redirect.as_deref(), Some("Bao the Whale"));
        assert_eq!(redirect.revisions[0].author, Author::Hidden);
        assert!(dump.next_page().unwrap().is_none());
    }
}

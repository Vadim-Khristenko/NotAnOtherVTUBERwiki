//! MediaWiki import: wikitext to the engine's Markdown, and XML dumps read
//! page by page, so a wiki on FANDOM or any MediaWiki can move here.
//!
//! [`convert`] turns one page's wikitext into Markdown and says what it
//! could not carry over in a [`Report`], so an import can show its coverage
//! before anything is written. Template calls stay as they are: the engine's
//! templates take the same `{{Name|key=value}}` calls and `{{#if:}}`, so a
//! template page imported next to the articles keeps working once its own
//! body is converted.
//!
//! Pure: no database, no network. The command line drives an import.

mod convert;
pub mod dump;
mod infobox;
pub mod slug;

pub use convert::{Converted, Options, Report, Unsupported, convert};
pub use infobox::image_params;

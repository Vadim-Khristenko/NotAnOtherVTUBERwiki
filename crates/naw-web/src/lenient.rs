//! Query string numbers that forgive: `?page=abc` reads as no page, not as a
//! 400 with a parser's message. An address is typed and shared by people,
//! and a wrong number in one should land on the first page, not an error.

use std::str::FromStr;

use serde::{Deserialize, Deserializer};

/// `#[serde(default, deserialize_with = "crate::lenient::number")]` on an
/// `Option<T>`: the value parsed, or `None` when it is not a `T`.
pub(crate) fn number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
{
    let raw: Option<String> = Option::deserialize(deserializer)?;
    Ok(raw.and_then(|s| s.trim().parse().ok()))
}

#[cfg(test)]
mod tests {
    #[derive(serde::Deserialize, Debug, Default)]
    struct Q {
        #[serde(default, deserialize_with = "super::number")]
        page: Option<i64>,
        #[serde(default, deserialize_with = "super::number")]
        n: Option<u8>,
    }

    fn parse(q: &str) -> Q {
        let uri: axum::http::Uri = format!("/x?{q}").parse().expect("uri");
        axum::extract::Query::<Q>::try_from_uri(&uri)
            .expect("never refuses")
            .0
    }

    #[test]
    fn a_number_that_is_not_one_is_none() {
        assert_eq!(parse("page=3").page, Some(3));
        assert_eq!(parse("page=%203%20").page, Some(3));
        assert_eq!(parse("page=abc").page, None);
        assert_eq!(parse("page=").page, None);
        assert_eq!(parse("page=99999999999999999999999").page, None);
        assert_eq!(parse("n=300").n, None);
        assert_eq!(parse("n=-1").n, None);
        assert_eq!(parse("").page, None);
        assert_eq!(parse("page=1%00").page, None);
    }
}

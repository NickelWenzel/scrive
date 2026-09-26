//! Results that name documents by URI.
//!
//! URIs decode as strings and parse per entry, because an entry whose URI does not parse is
//! skipped, while lsp-types' `Uri` fails the whole result. Everything else decodes strictly.

use serde::Deserialize;

use crate::uri;

/// A `textDocument/definition` result that is not `null`: a `Location`, a `Location[]` or a
/// `LocationLink[]`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Locations {
    One(Location),
    Many(Vec<Location>),
}

/// One entry of a definition result, with its URI unparsed.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Location {
    #[serde(rename_all = "camelCase")]
    Link {
        target_uri: String,
        target_selection_range: lsp_types::Range,
    },
    Plain {
        uri: String,
        range: lsp_types::Range,
    },
}

impl Locations {
    /// The first entry whose URI parses, as its document and range. A `LocationLink` points at
    /// its `targetSelectionRange`, the name rather than the whole definition.
    pub(crate) fn first(self) -> Option<(uri::Key, lsp_types::Range)> {
        let entries = match self {
            Locations::One(entry) => vec![entry],
            Locations::Many(entries) => entries,
        };
        entries.into_iter().find_map(|entry| {
            let (uri, range) = match entry {
                Location::Link {
                    target_uri,
                    target_selection_range,
                } => (target_uri, target_selection_range),
                Location::Plain { uri, range } => (uri, range),
            };
            let uri = uri.parse::<lsp_types::Uri>().ok()?;
            Some((uri::normalize(&uri), range))
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn range(start: u32, end: u32) -> Value {
        json!({"start": {"line": 0, "character": start}, "end": {"line": 0, "character": end}})
    }

    fn first(result: Value) -> Option<(String, lsp_types::Range)> {
        serde_json::from_value::<Option<Locations>>(result)
            .expect("the result decodes")
            .and_then(Locations::first)
            .map(|(key, range)| (key.as_str().to_owned(), range))
    }

    /// An entry whose URI does not parse is skipped, and the next one answers.
    #[test]
    fn first_parseable_location_wins() {
        let (key, found) = first(json!([
            {"uri": "file:///bad path.rs", "range": range(0, 1)},
            {"uri": "file:///w/b.rs", "range": range(3, 8)},
        ]))
        .expect("the second entry parses");
        assert_eq!(key, "file:///w/b.rs", "the parseable entry wins");
        assert_eq!(found.start.character, 3, "with its own range");
        assert_eq!(first(json!(null)), None, "null has no location");
        assert_eq!(first(json!([])), None, "an empty list has no location");
    }

    /// A single `Location` and a `LocationLink` list both decode; a link points at its name.
    #[test]
    fn single_locations_and_links_decode() {
        let (key, _) = first(json!({"uri": "file:///w/a.rs", "range": range(0, 1)}))
            .expect("a single location decodes");
        assert_eq!(key, "file:///w/a.rs", "the location's URI");
        let (_, found) = first(json!([{
            "targetUri": "file:///w/b.rs",
            "targetRange": range(0, 13),
            "targetSelectionRange": range(3, 8),
        }]))
        .expect("a link decodes");
        assert_eq!(
            found,
            serde_json::from_value(range(3, 8)).expect("range"),
            "the selection range"
        );
    }

    /// A result of the wrong shape is a decode error, not an absent definition.
    #[test]
    fn malformed_locations_do_not_decode() {
        for result in [json!("x"), json!([{"uri": "file:///w/a.rs"}])] {
            assert!(
                serde_json::from_value::<Option<Locations>>(result.clone()).is_err(),
                "{result} does not decode",
            );
        }
    }
}

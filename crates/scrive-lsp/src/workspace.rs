//! Results that name documents by URI: definition locations and workspace edits.
//!
//! URIs decode as strings and parse per entry, because an entry whose URI does not parse is
//! skipped, while lsp-types' `Uri` fails the whole result. Everything else decodes strictly: a
//! rename that half decoded would be half a rename.

use lsp_types::request::Request;
use serde::Deserialize;
use serde_json::Value;

use crate::{uri, Error};

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

/// A decoded `WorkspaceEdit`: text edits grouped per file, in the server's order. Resource
/// operations never get this far.
pub(crate) struct Edit {
    files: Vec<File>,
}

/// One file's edits, with the version the server says it edited: `None` for `version: null` and
/// for the `changes` map.
pub(crate) struct File {
    key: uri::Key,
    version: Option<i32>,
    edits: Vec<lsp_types::TextEdit>,
}

/// A `TextDocumentEdit` with its URI unparsed. An `AnnotatedTextEdit` carries `TextEdit`'s
/// fields, so it decodes as one, without its `annotationId`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocumentEdit {
    text_document: Document,
    edits: Vec<lsp_types::TextEdit>,
}

#[derive(Deserialize)]
struct Document {
    uri: String,
    version: Option<i32>,
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

impl Edit {
    /// Decodes a `textDocument/rename` result; `Ok(None)` for `null`. When both
    /// `documentChanges` and `changes` are present, `documentChanges` is used.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when `documentChanges` holds a create, rename or delete operation;
    /// [`Error::StaleEdit`] when two entries for one file name different versions;
    /// [`Error::Decode`] when anything else is malformed.
    pub(crate) fn decode(result: Value) -> Result<Option<Self>, Error> {
        let mut edit = match result {
            Value::Null => return Ok(None),
            Value::Object(_) => Self { files: Vec::new() },
            _ => return Err(malformed("a workspace edit is an object")),
        };
        if let Some(changes) = result.get("documentChanges") {
            let entries = changes
                .as_array()
                .ok_or_else(|| malformed("`documentChanges` is an array"))?;
            for entry in entries {
                if let Some(kind) = entry.get("kind") {
                    return Err(Error::Unsupported {
                        operation: kind.as_str().unwrap_or_default().to_owned(),
                    });
                }
                let DocumentEdit {
                    text_document,
                    edits,
                } = DocumentEdit::deserialize(entry).map_err(decode)?;
                let Ok(uri) = text_document.uri.parse::<lsp_types::Uri>() else {
                    continue;
                };
                edit.merge(uri::normalize(&uri), text_document.version, edits)?;
            }
        } else if let Some(changes) = result.get("changes") {
            let map = changes
                .as_object()
                .ok_or_else(|| malformed("`changes` is an object"))?;
            let mut entries = Vec::with_capacity(map.len());
            for (uri, edits) in map {
                let edits = Vec::<lsp_types::TextEdit>::deserialize(edits).map_err(decode)?;
                let Ok(uri) = uri.parse::<lsp_types::Uri>() else {
                    continue;
                };
                entries.push((uri::normalize(&uri), edits));
            }
            // The map has no order of its own.
            entries.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
            for (key, edits) in entries {
                edit.merge(key, None, edits)?;
            }
        }
        Ok(Some(edit))
    }

    /// The files the edit touches.
    pub(crate) fn files(&self) -> &[File] {
        &self.files
    }

    /// The files the edit touches, taken apart.
    pub(crate) fn into_files(self) -> Vec<File> {
        self.files
    }

    /// Appends `edits` to `key`'s file.
    fn merge(
        &mut self,
        key: uri::Key,
        version: Option<i32>,
        edits: Vec<lsp_types::TextEdit>,
    ) -> Result<(), Error> {
        let Some(file) = self.files.iter_mut().find(|file| file.key == key) else {
            self.files.push(File {
                key,
                version,
                edits,
            });
            return Ok(());
        };
        match (file.version, version) {
            (Some(a), Some(b)) if a != b => return Err(Error::StaleEdit { uri: key }),
            (None, Some(_)) => file.version = version,
            _ => {}
        }
        file.edits.extend(edits);
        Ok(())
    }
}

impl File {
    /// The file's document.
    pub(crate) fn key(&self) -> &uri::Key {
        &self.key
    }

    /// The version the server says it edited.
    pub(crate) fn version(&self) -> Option<i32> {
        self.version
    }

    /// The document and its edits.
    pub(crate) fn into_parts(self) -> (uri::Key, Vec<lsp_types::TextEdit>) {
        (self.key, self.edits)
    }
}

fn decode(source: serde_json::Error) -> Error {
    Error::Decode {
        method: lsp_types::request::Rename::METHOD.to_owned(),
        source,
    }
}

/// A decode error for a result whose shape breaks `expected`.
fn malformed(expected: &str) -> Error {
    decode(serde::de::Error::custom(expected))
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

    fn text_edit(text: &str) -> Value {
        json!({"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "newText": text})
    }

    fn document_edit(uri: &str, version: Value, texts: &[&str]) -> Value {
        json!({"textDocument": {"uri": uri, "version": version},
            "edits": texts.iter().map(|t| text_edit(t)).collect::<Vec<_>>()})
    }

    fn files(result: Value) -> Vec<File> {
        Edit::decode(result)
            .expect("the edit decodes")
            .expect("the edit is not null")
            .into_files()
    }

    fn keys(files: &[File]) -> Vec<&str> {
        files.iter().map(|file| file.key().as_str()).collect()
    }

    fn texts(file: File) -> Vec<String> {
        file.into_parts()
            .1
            .into_iter()
            .map(|edit| edit.new_text)
            .collect()
    }

    /// `documentChanges` keep the server's order and versions, one file per URI.
    #[test]
    fn document_changes_decode_per_uri_in_server_order() {
        let files = files(json!({"documentChanges": [
            document_edit("file:///w/b.rs", json!(2), &["b"]),
            document_edit("file:///w/a.rs", json!(5), &["a"]),
        ]}));
        assert_eq!(
            keys(&files),
            ["file:///w/b.rs", "file:///w/a.rs"],
            "server order"
        );
        assert_eq!(
            files.iter().map(File::version).collect::<Vec<_>>(),
            [Some(2), Some(5)],
            "each file's version"
        );
    }

    /// A create, rename or delete refuses the whole edit, whatever comes before it.
    #[test]
    fn resource_operations_reject_the_whole_edit() {
        for kind in ["create", "rename", "delete"] {
            let result = Edit::decode(json!({"documentChanges": [
                document_edit("file:///w/a.rs", json!(1), &["a"]),
                {"kind": kind, "uri": "file:///w/n.rs"},
            ]}));
            assert!(
                matches!(&result, Err(Error::Unsupported { operation }) if operation == kind),
                "a {kind} is unsupported: {:?}",
                result.map(|edit| edit.map(|edit| edit.files.len())),
            );
        }
    }

    /// The `changes` map has no order, so files come sorted by URI, with no version.
    #[test]
    fn changes_map_decodes_sorted_by_uri() {
        let files = files(json!({"changes": {
            "file:///w/b.rs": [text_edit("b")],
            "file:///w/a.rs": [text_edit("a")],
        }}));
        assert_eq!(keys(&files), ["file:///w/a.rs", "file:///w/b.rs"], "sorted");
        assert!(
            files.iter().all(|file| file.version().is_none()),
            "no versions"
        );
    }

    /// Two entries for one file become one file, edits in entry order; conflicting versions are
    /// stale.
    #[test]
    fn several_edits_for_one_uri_merge_in_order() {
        let mut files = files(json!({"documentChanges": [
            document_edit("file:///w/a.rs", json!(5), &["1", "2"]),
            document_edit("file:///w/a.rs", json!(5), &["3"]),
        ]}));
        assert_eq!(files.len(), 1, "one file");
        assert_eq!(texts(files.remove(0)), ["1", "2", "3"], "entry order");
        let result = Edit::decode(json!({"documentChanges": [
            document_edit("file:///w/a.rs", json!(5), &["1"]),
            document_edit("file:///w/a.rs", json!(6), &["2"]),
        ]}));
        assert!(
            matches!(&result, Err(Error::StaleEdit { uri }) if uri.as_str() == "file:///w/a.rs"),
            "two versions of one file are stale",
        );
    }

    /// An `AnnotatedTextEdit` decodes as its text edit, and `version: null` as no version.
    #[test]
    fn annotated_edits_and_null_versions_decode() {
        let mut files = files(json!({"documentChanges": [{
            "textDocument": {"uri": "file:///w/a.rs", "version": null},
            "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "newText": "x", "annotationId": "a1"}],
        }]}));
        assert_eq!(files[0].version(), None, "a null version");
        assert_eq!(texts(files.remove(0)), ["x"], "the annotated edit");
    }

    /// An entry whose URI does not parse is skipped, in either shape.
    #[test]
    fn entries_with_unparseable_uris_are_skipped() {
        let files_changed = files(json!({"documentChanges": [
            document_edit("file:///bad path.rs", json!(1), &["x"]),
            document_edit("file:///w/a.rs", json!(1), &["a"]),
        ]}));
        assert_eq!(
            keys(&files_changed),
            ["file:///w/a.rs"],
            "only the good entry"
        );
        let files_mapped = files(json!({"changes": {
            "file:///bad path.rs": [text_edit("x")],
            "file:///w/a.rs": [text_edit("a")],
        }}));
        assert_eq!(keys(&files_mapped), ["file:///w/a.rs"], "only the good key");
    }

    /// `null` is no edit, and a malformed edit is a decode error.
    #[test]
    fn null_and_malformed_edits() {
        assert!(
            matches!(Edit::decode(json!(null)), Ok(None)),
            "null is no edit"
        );
        for result in [
            json!("x"),
            json!({"documentChanges": "x"}),
            json!({"changes": {"file:///w/a.rs": "x"}}),
        ] {
            assert!(
                matches!(Edit::decode(result.clone()), Err(Error::Decode { .. })),
                "{result} does not decode",
            );
        }
    }
}

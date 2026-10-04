//! Rope text for tree-sitter: the parser's input callback and the query
//! cursor's [`TextProvider`], both reading chunks in place, so neither a
//! parse nor a `#eq?` / `#match?` predicate materializes the document.

use tree_sitter::{Node, TextProvider};

use crate::rope::Rope;

/// The parser's input: the text from `byte` to the end of its chunk, empty
/// past the end of the rope.
pub(crate) fn chunk_from(rope: &Rope, byte: usize) -> &[u8] {
    let Ok(byte) = u32::try_from(byte) else { return &[] };
    if byte >= rope.len() {
        return &[];
    }
    let (chunk, start) = rope.chunk_at(byte);
    &chunk.as_bytes()[(byte - start) as usize..]
}

/// A [`TextProvider`] yielding a node's text as the rope chunks it spans.
#[derive(Clone, Copy)]
pub(crate) struct RopeText<'a>(pub(crate) &'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeText<'a> {
    type I = Chunks<'a>;

    fn text(&mut self, node: Node) -> Self::I {
        let end = (node.end_byte() as u32).min(self.0.len());
        Chunks { rope: self.0, pos: node.start_byte() as u32, end }
    }
}

/// The chunk slices covering `pos..end`, in order.
pub(crate) struct Chunks<'a> {
    rope: &'a Rope,
    pos: u32,
    end: u32,
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }
        let (chunk, start) = self.rope.chunk_at(self.pos);
        let to = self.end.min(start + chunk.len() as u32);
        let slice = &chunk.as_bytes()[(self.pos - start) as usize..(to - start) as usize];
        debug_assert!(!slice.is_empty(), "a chunk holding `pos` reaches past it");
        self.pos = to;
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_from_walks_the_rope_to_its_end() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let rope = Rope::from_str(&text);
        let mut read = Vec::new();
        loop {
            let chunk = chunk_from(&rope, read.len());
            if chunk.is_empty() {
                break;
            }
            read.extend_from_slice(chunk);
        }
        assert_eq!(read, text.as_bytes());
        assert!(chunk_from(&rope, usize::MAX).is_empty());
    }

    #[test]
    fn chunks_cover_a_range_across_chunk_seams() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let rope = Rope::from_str(&text);
        let chunks = Chunks { rope: &rope, pos: 100, end: 1_500 };
        assert_eq!(chunks.flatten().copied().collect::<Vec<u8>>(), text.as_bytes()[100..1_500]);
    }
}

//! The LSP base protocol on a byte stream: `Content-Length` header blocks and the bodies they
//! announce, with whatever else a server prints between them kept apart as noise.

use std::mem;

/// The most a header block may hold before it is abandoned as noise.
const HEADER_LIMIT: usize = 8 * 1024;
/// The most of one noise line kept. The rest of the line, up to its `\n`, is dropped.
pub(crate) const LINE_LIMIT: usize = 64 * 1024;
/// Bodies up to this length are decoded.
pub(crate) const BODY_LIMIT: u64 = 64 * 1024 * 1024;
/// Bodies up to this length are streamed through and dropped. A longer length is garbage.
const SKIP_LIMIT: u64 = 1024 * 1024 * 1024;

/// Splits a server's stdout into frame bodies and the noise between them.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    /// Unconsumed bytes: the current line or header block, or the body being filled.
    buffer: Vec<u8>,
    /// Where the current line starts in `buffer`.
    line: usize,
    /// How far into `buffer` is known to hold no `\n` after `line`.
    scanned: usize,
    state: State,
}

#[derive(Debug, Default)]
enum State {
    /// Between frames. Lines are noise until a `Content-Length` line opens a block.
    #[default]
    Noise,
    /// Dropping the rest of a noise line longer than `LINE_LIMIT`, up to its `\n`.
    Overlong,
    /// Inside a header block that starts at `buffer[start]`.
    Header { start: usize, length: u64 },
    /// Filling a body of `length` bytes. `buffer` holds its first bytes and nothing else.
    Body { length: usize },
    /// Dropping the rest of a body too large to decode.
    Skip { remaining: u64 },
    /// A garbage length ended the stream. Nothing more decodes.
    Corrupt,
}

/// One thing a read produced, in stream order.
#[derive(Debug, PartialEq)]
pub(crate) enum Item {
    /// A complete body.
    Body(Vec<u8>),
    /// One line of non-LSP output, decoded lossily and capped at `LINE_LIMIT`.
    Noise(String),
    /// A body longer than 64 MiB began; it is dropped as it streams past.
    Skipped { length: u64 },
    /// A length above 1 GiB: the stream is not LSP any more, and the connection ends.
    Corrupt { length: u64 },
}

/// What one complete line is, for the header rules.
enum Line {
    /// Empty after its `\r` is stripped.
    Blank,
    /// `Content-Length: <u64>`, name compared ignoring ASCII case.
    Length(u64),
    /// Any other `name: value` line with a non-empty name.
    Header,
    /// Anything else, including `Content-Length` with a value that is not a number.
    Other,
}

/// What a declared body length means.
#[derive(Debug, PartialEq)]
enum Size {
    Decode(usize),
    Skip,
    Corrupt,
}

/// Whether the scan goes on after a header block's blank line.
enum Entered {
    /// The body was already buffered whole; the bytes after it are lines again.
    Scanning,
    /// The body, the skip or the corruption waits for more input.
    Waiting,
}

impl Decoder {
    /// Feeds one read. Returns what it completed, in stream order.
    pub(crate) fn push(&mut self, mut input: &[u8]) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            match &mut self.state {
                State::Corrupt => return items,
                State::Skip { remaining } => {
                    let dropped = usize::try_from(*remaining)
                        .map_or(input.len(), |left| left.min(input.len()));
                    input = &input[dropped..];
                    *remaining -= dropped as u64;
                    if *remaining > 0 {
                        return items;
                    }
                    self.state = State::Noise;
                }
                State::Body { length } => {
                    let length = *length;
                    let missing = (length - self.buffer.len()).min(input.len());
                    self.buffer.extend_from_slice(&input[..missing]);
                    input = &input[missing..];
                    if self.buffer.len() < length {
                        return items;
                    }
                    items.push(Item::Body(mem::take(&mut self.buffer)));
                    self.state = State::Noise;
                }
                State::Noise | State::Overlong | State::Header { .. } => {
                    if input.is_empty() {
                        return items;
                    }
                    self.buffer.extend_from_slice(input);
                    input = &[];
                    self.scan(&mut items);
                }
            }
        }
    }

    /// The stream ended: a trailing noise line without `\n`, if any. A partial header block or
    /// body is dropped, since the connection is ending anyway.
    pub(crate) fn finish(self) -> Option<String> {
        match self.state {
            State::Noise => noise(&self.buffer[self.line..]),
            State::Overlong
            | State::Header { .. }
            | State::Body { .. }
            | State::Skip { .. }
            | State::Corrupt => None,
        }
    }

    /// The buffer's allocation, which must stay bounded however long the stream runs.
    #[cfg(test)]
    fn buffered(&self) -> usize {
        self.buffer.capacity()
    }

    /// Applies the line rules to every complete line in `buffer`, then the partial-line rules to
    /// what is left. Returns early once a body, a skip or the corruption takes over.
    fn scan(&mut self, items: &mut Vec<Item>) {
        loop {
            let Some(found) = self.buffer[self.scanned..].iter().position(|&b| b == b'\n') else {
                self.scanned = self.buffer.len();
                self.partial(items);
                self.compact();
                return;
            };
            let end = self.scanned + found;
            let next = end + 1;
            match self.state {
                State::Overlong => self.state = State::Noise,
                State::Noise => match classify(strip(&self.buffer[self.line..end])) {
                    Line::Length(length) => {
                        self.state = State::Header {
                            start: self.line,
                            length,
                        };
                    }
                    Line::Blank => {}
                    Line::Header | Line::Other => {
                        items.extend(noise(&self.buffer[self.line..end]).map(Item::Noise));
                    }
                },
                State::Header { start, length } => {
                    match classify(strip(&self.buffer[self.line..end])) {
                        Line::Length(fresh) => {
                            self.abandon(start, self.line, items);
                            self.state = State::Header {
                                start: self.line,
                                length: fresh,
                            };
                        }
                        Line::Header => {
                            if next - start > HEADER_LIMIT {
                                self.abandon(start, next, items);
                                self.state = State::Noise;
                            }
                        }
                        Line::Blank => {
                            self.line = next;
                            self.scanned = next;
                            match self.enter(length, items) {
                                Entered::Scanning => continue,
                                Entered::Waiting => return,
                            }
                        }
                        Line::Other => {
                            self.abandon(start, next, items);
                            self.state = State::Noise;
                        }
                    }
                }
                State::Body { .. } | State::Skip { .. } | State::Corrupt => {
                    unreachable!("a body state never scans lines")
                }
            }
            self.line = next;
            self.scanned = next;
        }
    }

    /// The rules for the unterminated line at the end of `buffer`: a header block past its cap
    /// is abandoned, and a noise line past its cap is cut.
    fn partial(&mut self, items: &mut Vec<Item>) {
        if let State::Header { start, .. } = self.state {
            if self.buffer.len() - start > HEADER_LIMIT {
                self.abandon(start, self.line, items);
                self.state = State::Noise;
            }
        }
        match self.state {
            State::Noise => {
                if self.buffer.len() - self.line > LINE_LIMIT {
                    let cut = self.line + LINE_LIMIT;
                    items.extend(noise(&self.buffer[self.line..cut]).map(Item::Noise));
                    self.buffer.truncate(self.line);
                    self.scanned = self.line;
                    self.state = State::Overlong;
                }
            }
            State::Overlong => {
                self.buffer.truncate(self.line);
                self.scanned = self.line;
            }
            State::Header { .. } | State::Body { .. } | State::Skip { .. } | State::Corrupt => {}
        }
    }

    /// Releases the consumed prefix of `buffer`, once per push.
    fn compact(&mut self) {
        let keep = match &mut self.state {
            State::Header { start, .. } => mem::take(start),
            State::Noise | State::Overlong => self.line,
            State::Body { .. } | State::Skip { .. } | State::Corrupt => return,
        };
        self.buffer.drain(..keep);
        self.line -= keep;
        self.scanned -= keep;
    }

    /// Turns the complete lines of `buffer[start..end]`, an abandoned header block, into noise.
    fn abandon(&self, start: usize, end: usize, items: &mut Vec<Item>) {
        let block = &self.buffer[start..end];
        let lines = block
            .strip_suffix(b"\n")
            .unwrap_or(block)
            .split(|&b| b == b'\n');
        items.extend(lines.filter_map(noise).map(Item::Noise));
    }

    /// Starts the body a header block announced. Everything before `line` is header and goes;
    /// what follows is the body's first bytes.
    fn enter(&mut self, length: u64, items: &mut Vec<Item>) -> Entered {
        self.buffer.drain(..self.line);
        self.line = 0;
        self.scanned = 0;
        match Size::of(length) {
            Size::Decode(length) => {
                if self.buffer.len() >= length {
                    let tail = self.buffer.split_off(length);
                    items.push(Item::Body(mem::replace(&mut self.buffer, tail)));
                    self.state = State::Noise;
                    return Entered::Scanning;
                }
                self.buffer.reserve_exact(length - self.buffer.len());
                self.state = State::Body { length };
                Entered::Waiting
            }
            Size::Skip => {
                items.push(Item::Skipped { length });
                let buffered = self.buffer.len();
                let dropped = usize::try_from(length).map_or(buffered, |left| left.min(buffered));
                self.buffer.drain(..dropped);
                let remaining = length - dropped as u64;
                if remaining == 0 {
                    self.state = State::Noise;
                    return Entered::Scanning;
                }
                self.state = State::Skip { remaining };
                Entered::Waiting
            }
            Size::Corrupt => {
                items.push(Item::Corrupt { length });
                self.buffer = Vec::new();
                self.state = State::Corrupt;
                Entered::Waiting
            }
        }
    }
}

impl Size {
    fn of(length: u64) -> Self {
        if length <= BODY_LIMIT {
            Size::Decode(usize::try_from(length).expect("64 MiB fits in usize"))
        } else if length <= SKIP_LIMIT {
            Size::Skip
        } else {
            Size::Corrupt
        }
    }
}

/// `body` framed for the wire: header and body in one buffer, for one `write_all`.
pub(crate) fn encode(body: &[u8]) -> Vec<u8> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(body);
    frame
}

/// `line` without the one `\r` before its `\n`.
fn strip(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn classify(line: &[u8]) -> Line {
    if line.is_empty() {
        return Line::Blank;
    }
    let Some(colon) = line.iter().position(|&b| b == b':') else {
        return Line::Other;
    };
    let name = line[..colon].trim_ascii();
    if name.is_empty() {
        return Line::Other;
    }
    if !name.eq_ignore_ascii_case(b"content-length") {
        return Line::Header;
    }
    let length = std::str::from_utf8(&line[colon + 1..])
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    length.map_or(Line::Other, Line::Length)
}

/// One noise line as text: its `\r` stripped, capped, decoded lossily. `None` when blank.
fn noise(line: &[u8]) -> Option<String> {
    let line = strip(line);
    let line = &line[..line.len().min(LINE_LIMIT)];
    (!line.is_empty()).then(|| String::from_utf8_lossy(line).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str) -> Item {
        Item::Body(text.as_bytes().to_vec())
    }

    fn noise_item(text: &str) -> Item {
        Item::Noise(text.to_owned())
    }

    /// Everything one fresh decoder makes of `pushes`, one list per push.
    fn decode(pushes: &[&[u8]]) -> Vec<Vec<Item>> {
        let mut decoder = Decoder::default();
        pushes.iter().map(|input| decoder.push(input)).collect()
    }

    fn json(text: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "window/showMessage",
            "params": { "type": 3, "message": text },
        }))
        .expect("the fixture serializes")
    }

    /// A decoded frame is the body that was encoded. The body holds non-ASCII text, so the
    /// length counts bytes.
    #[test]
    fn a_frame_round_trips() {
        let sent = json("héllo, wörld");
        assert_eq!(
            decode(&[&encode(&sent)]),
            [vec![Item::Body(sent)]],
            "the body survives the wire"
        );
    }

    /// A frame that arrives one byte at a time decodes only once its last byte is in.
    #[test]
    fn a_frame_split_across_reads_waits_for_its_last_byte() {
        let sent = json("split");
        let wire = encode(&sent);
        let mut decoder = Decoder::default();
        for (index, byte) in wire.iter().enumerate() {
            let items = decoder.push(std::slice::from_ref(byte));
            if index + 1 < wire.len() {
                assert!(items.is_empty(), "byte {index} does not complete the frame");
            } else {
                assert_eq!(
                    items,
                    [Item::Body(sent.clone())],
                    "the last byte completes it"
                );
            }
        }
    }

    /// Two frames and the start of a third in one read decode in order; the third completes
    /// with the next read.
    #[test]
    fn frames_in_one_read_decode_in_order() {
        let (first, second, third) = (json("one"), json("two"), json("three"));
        let mut wire = encode(&first);
        wire.extend(encode(&second));
        let third_wire = encode(&third);
        wire.extend(&third_wire[..10]);
        assert_eq!(
            decode(&[&wire, &third_wire[10..]]),
            [
                vec![Item::Body(first), Item::Body(second)],
                vec![Item::Body(third)]
            ],
            "the frames arrive in order"
        );
    }

    /// A header block without `Content-Length` is noise, and its body is never decoded.
    #[test]
    fn a_header_block_without_content_length_is_noise() {
        assert_eq!(
            decode(&[b"Content-Type: application/vscode-jsonrpc\r\n\r\n{}"]),
            [vec![noise_item("Content-Type: application/vscode-jsonrpc")]],
            "no block opens"
        );
    }

    /// Lines that are not headers, and a length that is not a number, are noise, and a valid
    /// frame after them decodes.
    #[test]
    fn garbage_headers_are_noise_and_the_next_frame_decodes() {
        for garbage in ["hello", "Content-Length: many"] {
            let wire = format!("{garbage}\r\n\r\nContent-Length: 2\r\n\r\n{{}}");
            assert_eq!(
                decode(&[wire.as_bytes()]),
                [vec![noise_item(garbage), body("{}")]],
                "{garbage:?} is noise"
            );
        }
    }

    /// Output before the first frame is noise, and the frame still decodes.
    #[test]
    fn a_banner_before_the_first_frame_is_noise() {
        assert_eq!(
            decode(&[b"banner\nContent-Length: 2\r\n\r\n{}"]),
            [vec![noise_item("banner"), body("{}")]],
            "the decoder resyncs past the banner"
        );
    }

    /// Output between frames is noise, and the next frame decodes.
    #[test]
    fn garbage_between_frames_resyncs() {
        assert_eq!(
            decode(&[b"Content-Length: 2\r\n\r\n{}junk\nContent-Length: 2\r\n\r\n[]"]),
            [vec![body("{}"), noise_item("junk"), body("[]")]],
            "the junk line is noise"
        );
    }

    /// Header names match without regard to case or spacing, and lines may end in `\n` alone.
    #[test]
    fn lf_only_header_lines_decode() {
        assert_eq!(
            decode(&[b"content-length:2\n\n{}"]),
            [vec![body("{}")]],
            "the frame decodes"
        );
        assert_eq!(
            decode(&[
                b"Content-Length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}"
            ]),
            [vec![body("{}")]],
            "other headers are ignored"
        );
    }

    /// A `Content-Length` line opens a fresh block, so a log line with a `:` never swallows a
    /// real header.
    #[test]
    fn a_content_length_line_opens_a_fresh_block() {
        assert_eq!(
            decode(&[b"Content-Length: 2\r\nINFO: x\r\nContent-Length: 3\r\n\r\n{} "]),
            [vec![
                noise_item("Content-Length: 2"),
                noise_item("INFO: x"),
                body("{} ")
            ]],
            "the second length wins"
        );
        assert_eq!(
            decode(&[b"Content-Length: 2\r\nhello\r\n\r\n{}"]),
            [vec![noise_item("Content-Length: 2"), noise_item("hello")]],
            "a line that is no header abandons the block, and `{{}}` waits as a partial line"
        );
    }

    /// A header block past 8 KiB is abandoned: its lines become noise, and so do the header
    /// lines after it.
    #[test]
    fn a_header_block_past_eight_kib_is_abandoned() {
        let mut wire = b"Content-Length: 2\r\n".to_vec();
        for _ in 0..9000 {
            wire.extend_from_slice(b"X: y\r\n");
        }
        let items: Vec<Item> = decode(&[&wire]).into_iter().flatten().collect();
        assert_eq!(
            items[0],
            noise_item("Content-Length: 2"),
            "the opening line"
        );
        assert_eq!(items.len(), 9001, "every line is noise");
        assert!(
            items[1..].iter().all(|item| *item == noise_item("X: y")),
            "each header line is noise"
        );

        let mut decoder = Decoder::default();
        let mut items = decoder.push(b"Content-Length: 2\r\n");
        items.extend(decoder.push(&[b'x'; HEADER_LIMIT]));
        assert_eq!(
            items,
            [noise_item("Content-Length: 2")],
            "an unterminated line past the cap abandons the block"
        );
        assert_eq!(
            decoder.push(b"\n"),
            [noise_item(&"x".repeat(HEADER_LIMIT))],
            "the unterminated line goes on as noise"
        );
    }

    /// A noise line past 64 KiB is cut there, and the rest of it is dropped up to its `\n`.
    #[test]
    fn an_overlong_noise_line_is_capped_and_its_tail_dropped() {
        let long = vec![b'a'; 70_000];
        assert_eq!(
            decode(&[&long, b"bbb\nContent-Length: 2\r\n\r\n{}"]),
            [vec![noise_item(&"a".repeat(LINE_LIMIT))], vec![body("{}")]],
            "the head is kept, the tail dropped"
        );
        let mut complete = vec![b'c'; 70_000];
        complete.push(b'\n');
        assert_eq!(
            decode(&[&complete]),
            [vec![noise_item(&"c".repeat(LINE_LIMIT))]],
            "a complete line is capped too"
        );
    }

    /// A stream of nothing but newlines produces nothing and keeps its buffer small.
    #[test]
    fn a_newline_only_stream_stays_bounded() {
        let chunk = vec![b'\n'; 64 * 1024];
        let mut decoder = Decoder::default();
        for _ in 0..(100_000_000 / chunk.len()) {
            assert!(decoder.push(&chunk).is_empty(), "blank lines are dropped");
            assert!(decoder.buffered() <= 256 * 1024, "the buffer stays small");
        }
    }

    /// A body over 64 MiB is reported, dropped as it streams past, and the frame after it
    /// decodes.
    #[test]
    fn a_body_over_64_mib_is_skipped_and_the_next_frame_decodes() {
        let length: u64 = 100 * 1024 * 1024;
        let mut decoder = Decoder::default();
        let header = format!("Content-Length: {length}\r\n\r\n");
        assert_eq!(
            decoder.push(header.as_bytes()),
            [Item::Skipped { length }],
            "the skip is reported when it starts"
        );
        let chunk = vec![0; 64 * 1024];
        let mut left = usize::try_from(length).expect("100 MiB fits");
        while left > 0 {
            let part = left.min(chunk.len());
            assert!(
                decoder.push(&chunk[..part]).is_empty(),
                "skipped bytes yield nothing"
            );
            assert!(decoder.buffered() <= 256 * 1024, "the buffer stays small");
            left -= part;
        }
        assert_eq!(
            decoder.push(b"Content-Length: 2\r\n\r\n{}"),
            [body("{}")],
            "the next frame decodes"
        );
    }

    /// A length over 1 GiB ends the stream: nothing after it decodes.
    #[test]
    fn a_length_over_1_gib_corrupts_the_stream() {
        let length = SKIP_LIMIT + 1;
        let header = format!("Content-Length: {length}\r\n\r\n{{}}");
        assert_eq!(
            decode(&[header.as_bytes(), b"Content-Length: 2\r\n\r\n{}"]),
            [vec![Item::Corrupt { length }], vec![]],
            "the stream is over"
        );
    }

    /// The limits are inclusive.
    #[test]
    fn size_boundaries_match_the_limits() {
        assert_eq!(
            Size::of(BODY_LIMIT),
            Size::Decode(64 * 1024 * 1024),
            "64 MiB decodes"
        );
        assert_eq!(Size::of(BODY_LIMIT + 1), Size::Skip, "past 64 MiB skips");
        assert_eq!(Size::of(SKIP_LIMIT), Size::Skip, "1 GiB skips");
        assert_eq!(
            Size::of(SKIP_LIMIT + 1),
            Size::Corrupt,
            "past 1 GiB is garbage"
        );
    }

    /// `Content-Length: 0` is an empty body.
    #[test]
    fn a_zero_length_body_is_an_empty_body() {
        assert_eq!(
            decode(&[b"Content-Length: 0\r\n\r\n"]),
            [vec![body("")]],
            "an empty body"
        );
    }

    /// A body's length counts bytes, a character split across reads stays intact, and the
    /// bytes after the body are lines again.
    #[test]
    fn a_body_counts_bytes_across_reads() {
        assert_eq!(
            decode(&[b"Content-Length: 5\r\n\r\nh\xC3\xA9\xC3", b"\xA9!", b"\n"]),
            [vec![], vec![body("héé")], vec![noise_item("!")]],
            "the body is its five bytes"
        );
    }

    /// At EOF a trailing noise line is flushed, and a partial frame is dropped.
    #[test]
    fn eof_returns_the_trailing_noise_line_and_drops_a_partial_frame() {
        let mut decoder = Decoder::default();
        assert!(
            decoder.push(b"tail without newline").is_empty(),
            "nothing is complete"
        );
        assert_eq!(
            decoder.finish().as_deref(),
            Some("tail without newline"),
            "the line is flushed"
        );
        let mut decoder = Decoder::default();
        assert!(
            decoder.push(b"Content-Length: 9\r\n\r\n{\"a\"").is_empty(),
            "the body is partial"
        );
        assert_eq!(decoder.finish(), None, "the partial body is dropped");
    }

    /// The header and the body go out as one buffer.
    #[test]
    fn encode_writes_header_and_body_in_one_buffer() {
        let frame = encode(b"{}");
        assert_eq!(frame, b"Content-Length: 2\r\n\r\n{}", "header then body");
        assert_eq!(frame.capacity(), frame.len(), "one exact allocation");
    }
}

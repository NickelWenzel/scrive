//! Wall-clock and memory benchmarks for the tree-sitter highlight backend,
//! driven through `Document`'s public API like the `perf` bench. Results are
//! recorded in `benches/LEDGER.md`.
//!
//! Run: `cargo bench -p scrive-core --features tree-sitter --bench tree_sitter`.
//! Besides criterion's timings, it prints the calls each scenario takes and
//! the cost per call, the number `HIGHLIGHT_MAX_PARSE_CHECKS_PER_CALL` is
//! tuned against.
//!
//! With `SCRIVE_BENCH_MEMORY=1` it instead prints the live heap a parse tree
//! holds, counted by the allocator, and times nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion};
use scrive_core::{Document, Rgba, SelectionSet, SpanStyle, TokenTheme, TreeSitterDef};

/// The system allocator plus a count of live bytes, kept during the memory
/// run only. tree-sitter's C library
/// allocates with libc unless told otherwise, so the memory run routes it
/// through here as well (`install_tree_sitter_allocator`).
struct Counting;

/// Set by the memory run only, so timed runs pay no counting.
static COUNTING: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicIsize = AtomicIsize::new(0);
/// Live blocks the C library holds, each carrying a `c_alloc` header that
/// `LIVE` counts too.
static C_BLOCKS: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size() as isize);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(-(layout.size() as isize));
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size as isize - layout.size() as isize);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count(bytes: isize) {
    if COUNTING.load(Ordering::Relaxed) {
        LIVE.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Live heap bytes, not counting the `c_alloc` headers.
fn live_bytes() -> isize {
    LIVE.load(Ordering::Relaxed) - C_BLOCKS.load(Ordering::Relaxed) * c_alloc::HEADER as isize
}

/// C's `free` gets no size, so each C block carries its size in a header
/// in front of the pointer it hands out. 16 bytes keeps malloc's alignment.
mod c_alloc {
    use super::*;

    pub const HEADER: usize = 16;

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size + HEADER, HEADER).expect("C allocation size fits a Layout")
    }

    unsafe fn finish(base: *mut u8, size: usize) -> *mut c_void {
        assert!(!base.is_null(), "out of memory");
        unsafe {
            base.cast::<usize>().write(size);
            base.add(HEADER).cast()
        }
    }

    unsafe fn base(ptr: *mut c_void) -> (*mut u8, usize) {
        let base = unsafe { ptr.cast::<u8>().sub(HEADER) };
        (base, unsafe { base.cast::<usize>().read() })
    }

    pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
        C_BLOCKS.fetch_add(1, Ordering::Relaxed);
        unsafe { finish(GLOBAL.alloc(layout(size)), size) }
    }

    pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut c_void {
        let size = n.checked_mul(size).expect("calloc size overflows");
        C_BLOCKS.fetch_add(1, Ordering::Relaxed);
        unsafe { finish(GLOBAL.alloc_zeroed(layout(size)), size) }
    }

    pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        if ptr.is_null() {
            return unsafe { malloc(size) };
        }
        let (base, old) = unsafe { base(ptr) };
        unsafe { finish(GLOBAL.realloc(base, layout(old), size + HEADER), size) }
    }

    pub unsafe extern "C" fn free(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        let (base, size) = unsafe { base(ptr) };
        C_BLOCKS.fetch_sub(1, Ordering::Relaxed);
        unsafe { GLOBAL.dealloc(base, layout(size)) }
    }
}

fn install_tree_sitter_allocator() {
    let allocator = tree_sitter::Allocator {
        malloc: c_alloc::malloc,
        calloc: c_alloc::calloc,
        realloc: c_alloc::realloc,
        free: c_alloc::free,
    };
    // SAFETY: called once, from `main` before any tree-sitter object
    // exists; the four functions share one allocator.
    unsafe { tree_sitter::set_allocator(Some(allocator)) };
}

/// Seven rows of Rust: a function opening, a string binding, a comment, a
/// generic call, the closing `}`, a unit struct, a blank.
fn rows(n: usize) -> String {
    (0..n)
        .map(|i| match i % 7 {
            0 => format!("fn f{i}() {{"),
            1 => format!("    let s = \"text {i}\\n\";"),
            2 => "    // a note".to_string(),
            3 => format!("    let t: Vec<u8> = g{i}(s);"),
            4 => "}".to_string(),
            5 => format!("struct S{i};"),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// About `bytes` of top-level items: the worst shape for a reparse, which
/// steps over every reused top-level sibling.
fn flat(bytes: usize) -> String {
    // A row averages ~18 bytes.
    let mut text = rows(bytes / 18 + 7);
    text.truncate(text[..bytes.min(text.len())].rfind('\n').unwrap_or(0));
    text.push('\n');
    text
}

/// About `bytes` of the same rows wrapped in 1,400-row modules, the way real
/// code nests.
fn nested(bytes: usize) -> String {
    let body = rows(1_400);
    let mut text = String::with_capacity(bytes + body.len());
    let mut k = 0;
    while text.len() < bytes {
        text.push_str(&format!("mod m{k} {{\n{body}\n}}\n"));
        k += 1;
    }
    text
}

fn grammar() -> TreeSitterDef {
    TreeSitterDef::new(tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY)
        .expect("tree-sitter-rust's highlights query compiles")
}

fn theme() -> TokenTheme {
    let style = |r, g, b| SpanStyle { fg: Rgba { r, g, b, a: 0xff }, bold: false, italic: false };
    TokenTheme::builder()
        .capture("keyword", style(0xec, 0x6a, 0x88))
        .capture("string", style(0xa3, 0xc7, 0x6d))
        .capture("comment", style(0x6a, 0x6f, 0x7a))
        .capture("function", style(0xe0, 0xb6, 0x58))
        .capture("type", style(0x4f, 0xb6, 0xc7))
        .build()
}

/// A document with a tree-sitter grammar and its window around `viewport`,
/// not yet parsed.
fn doc(text: &str, viewport: std::ops::Range<u32>) -> Document {
    let mut d = Document::new(text).expect("bench doc loads");
    d.set_syntax(grammar(), theme());
    d.set_highlight_window(viewport);
    d
}

/// Drive the highlight to convergence the way the app's frame sweep does;
/// returns the calls it took.
fn converge(d: &mut Document, target: u32) -> u32 {
    let mut calls = 0;
    while d.highlight_frontier().is_some() {
        d.tokenize_highlight(target);
        calls += 1;
    }
    calls
}

/// A 40-row viewport in the middle of `text`'s rows.
fn mid_viewport(text: &str) -> std::ops::Range<u32> {
    let mid = text.bytes().filter(|&b| b == b'\n').count() as u32 / 2;
    mid..mid + 40
}

/// Put the caret inside the first numbered identifier at or after `row`.
fn caret_in_identifier(d: &mut Document, row: u32) {
    let (row, col) = (row..)
        .find_map(|r| Some((r, d.buffer().line(r).find(|c: char| c.is_ascii_digit())?)))
        .expect("a later row has a numbered identifier");
    let at = d.buffer().point_to_offset(scrive_core::Point::new(row, col as u32));
    d.set_selections(SelectionSet::new(at));
}

/// Drive to convergence like [`converge`], printing the per-call cost for
/// the ledger. Criterion times whole convergences; this shows how a single
/// frame fares.
fn report_calls(label: &str, d: &mut Document, target: u32) {
    let mut times = Vec::new();
    while d.highlight_frontier().is_some() {
        let t = Instant::now();
        d.tokenize_highlight(target);
        times.push(t.elapsed());
    }
    let total: Duration = times.iter().sum();
    let mut sorted = times.clone();
    sorted.sort();
    eprintln!(
        "tree_sitter/{label}: {} calls, {total:.1?} total, per call median {:.2?} p99 {:.2?} max {:.2?}",
        times.len(),
        sorted[sorted.len() / 2],
        sorted[sorted.len() * 99 / 100],
        sorted[sorted.len() - 1],
    );
    if times.len() <= 10 {
        eprintln!("  each: {times:.2?}");
    }
}

/// Live heap a parse holds, printed for the ledger. The tree alone is a
/// direct `tree_sitter::Parser` parse, measured after the parser is dropped.
/// The highlight-state figure is everything `set_syntax` plus a converged
/// highlight adds to a `Document`: tree, parser, query and window spans.
fn report_memory(label: &str, text: &str) {
    let (before, blocks) = (live_bytes(), C_BLOCKS.load(Ordering::Relaxed));
    let tree = {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).expect("tree-sitter-rust loads");
        parser.parse(text, None).expect("an uncancelled parse returns a tree")
    };
    let tree_bytes = live_bytes() - before;
    let tree_blocks = C_BLOCKS.load(Ordering::Relaxed) - blocks;
    drop(tree);

    let mut d = Document::new(text).expect("bench doc loads");
    let before = live_bytes();
    d.set_syntax(grammar(), theme());
    d.set_highlight_window(mid_viewport(text));
    converge(&mut d, u32::MAX);
    let state_bytes = live_bytes() - before;
    let mib = |b: isize| b as f64 / (1024.0 * 1024.0);
    eprintln!(
        "tree_sitter/memory_{label}: tree {:.1} MiB in {tree_blocks} blocks ({:.1}x text), highlight state {:.1} MiB",
        mib(tree_bytes),
        tree_bytes as f64 / text.len() as f64,
        mib(state_bytes),
    );
}

fn cold_parse(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree_sitter");
    g.sample_size(10).measurement_time(Duration::from_secs(5));
    for (label, bytes) in [("1m", 1_000_000), ("10m", 10_000_000)] {
        let text = nested(bytes);
        let viewport = mid_viewport(&text);
        report_calls(&format!("cold_parse_{label}"), &mut doc(&text, viewport.clone()), viewport.end);
        g.bench_function(format!("cold_parse_{label}"), |b| {
            b.iter_batched(
                || doc(&text, viewport.clone()),
                // Returning the document leaves its drop, tree included, to
                // criterion, outside the timing.
                |mut d| {
                    converge(&mut d, viewport.end);
                    d
                },
                BatchSize::PerIteration,
            );
        });
    }
}

fn keystroke(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree_sitter");
    g.sample_size(10).measurement_time(Duration::from_secs(3));
    let corpora = [
        ("nested_1m", nested(1_000_000)),
        ("flat_1m", flat(1_000_000)),
        ("flat_10m", flat(10_000_000)),
    ];
    for (label, text) in corpora {
        let viewport = mid_viewport(&text);
        let mut d = doc(&text, viewport.clone());
        converge(&mut d, viewport.end);
        caret_in_identifier(&mut d, viewport.start + 20);
        d.type_char('7');
        report_calls(&format!("keystroke_{label}"), &mut d, viewport.end);
        // Each iteration types one more digit into the same identifier, so
        // every keystroke reparses the same spot. The identifier and the undo
        // history grow across iterations; neither changes what a reparse
        // walks, and the history isn't touched by highlighting.
        g.bench_function(format!("keystroke_{label}"), |b| {
            b.iter(|| {
                d.type_char('7');
                converge(&mut d, viewport.end)
            });
        });
    }
}

fn window_jump(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree_sitter");
    g.sample_size(10).measurement_time(Duration::from_secs(3));
    let text = nested(1_000_000);
    let n = text.bytes().filter(|&b| b == b'\n').count() as u32;
    let mut d = doc(&text, 0..40);
    converge(&mut d, 40);
    // Alternate between a quarter and three quarters down, so every jump
    // lands outside the previous window and queries a full window of rows.
    let spots = [n / 4, 3 * n / 4];
    d.set_highlight_window(spots[0]..spots[0] + 40);
    report_calls("window_jump_1m", &mut d, spots[0] + 40);
    let mut i = 0;
    g.bench_function("window_jump_1m", |b| {
        b.iter(|| {
            i ^= 1;
            let top = spots[i];
            d.set_highlight_window(top..top + 40);
            converge(&mut d, top + 40)
        });
    });
}

fn main() {
    // Counting C allocations slows a parse by about a fifth, so the memory
    // run is separate from the timed one.
    if std::env::var("SCRIVE_BENCH_MEMORY").as_deref() == Ok("1") {
        // Nothing allocated before this point is freed during a measured
        // step, so starting the count late skews no delta.
        COUNTING.store(true, Ordering::Relaxed);
        install_tree_sitter_allocator();
        report_memory("1m", &nested(1_000_000));
        report_memory("10m", &nested(10_000_000));
        return;
    }
    let mut c = Criterion::default().configure_from_args();
    cold_parse(&mut c);
    keystroke(&mut c);
    window_jump(&mut c);
    c.final_summary();
}

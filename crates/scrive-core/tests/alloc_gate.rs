//! The heap-allocation half of the complexity gate (`src/perf_gate.rs`).
//!
//! The rope is ropey's, so `sum_tree::NODE_ALLOCS` cannot see its copy-on-write
//! node clones. This test counts every heap allocation instead, which the
//! library cannot do itself (`forbid(unsafe_code)` rules out a `GlobalAlloc`).
//! Op counts only, no wall-clock, so it holds on any machine.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use scrive_core::{Document, SelectionSet};

struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Bracket-free, so the bracket tree (which rebuilds itself for a large edit
/// batch) stays empty and the count is the rope's.
const BLOCK: &str = "let f = 1\n    mark = 0\nend\n";

/// Type once at `k` carets, one every `spacing` blocks of a `k * spacing`-block
/// document, and return the heap allocations per caret.
fn allocs_per_caret(k: usize, spacing: usize) -> f64 {
    let mut d = Document::new(&BLOCK.repeat(k * spacing)).unwrap();
    let carets: Vec<(u32, u32)> =
        (0..k).map(|i| (i * spacing * BLOCK.len()) as u32 + 8).map(|o| (o, o)).collect();
    d.set_selections(SelectionSet::from_ranges(&carets, 0));
    let before = ALLOCS.with(Cell::get);
    d.insert_text("a");
    (ALLOCS.with(Cell::get) - before) as f64 / k as f64
}

/// Multi-caret typing must allocate O(carets) on both rope paths: carets a
/// block apart cross the rope's rebuild threshold (one edit per 2 KiB) and
/// rebuild it once; carets 100 blocks (2.7 KB) apart splice each edit. A
/// quadratic shape reads ~4× per caret here. Both paths take ~1–2 allocations
/// per caret; a rope rebuild per caret would take one per ropey leaf (~1 KB of
/// document) per caret, which the ceiling catches.
#[test]
fn multicaret_typing_allocates_linearly() {
    for (path, spacing) in [("rebuild", 1), ("splice", 100)] {
        let (s, b) = (allocs_per_caret(400, spacing), allocs_per_caret(1600, spacing));
        eprintln!("[alloc_gate] type at N carets ({path}), allocs/caret {s:>7.1} -> {b:>7.1}  ({:.2}x)", b / s);
        assert!(b <= s * 1.8, "{path}: multi-caret typing allocates superlinearly: {s:.1} -> {b:.1}/caret");
        assert!(b <= 10.0, "{path}: multi-caret typing allocates {b:.1} times per caret");
    }
}

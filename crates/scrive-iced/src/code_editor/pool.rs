//! A [`CodeEditor`]'s off-thread highlight sweep for large documents, the
//! [`HighlightPool`] it owns. Each hook returns `false` when the document
//! highlights on the UI thread instead, and the caller then drives
//! `tokenize_highlight` itself. Without the `syntect` feature there is no pool
//! and every hook returns `false`.

#[cfg(feature = "syntect")]
use std::ops::Range;

use super::CodeEditor;
#[cfg(feature = "syntect")]
use crate::highlight_pool::{HighlightPool, PARALLEL_MIN_BYTES};

#[cfg(feature = "syntect")]
impl CodeEditor {
    /// Whether the document is large enough for the off-thread highlight pool
    /// (never on wasm32, which has no threads) and its grammar can run there.
    /// A tree-sitter document has no pool engine (`HighlightPool::new` builds
    /// nothing for it), so at any size it highlights on the UI thread, one
    /// budgeted call per frame.
    pub(super) fn uses_pool(&self) -> bool {
        PARALLEL_MIN_BYTES.is_some_and(|min| self.doc.buffer().len() >= min)
            && self.doc.highlight_engine().is_some()
    }

    /// Aim the pool at the new viewport `rows`, creating it on first sight.
    pub(super) fn pool_viewport(&mut self, rows: Range<u32>) -> bool {
        if !self.uses_pool() {
            return false;
        }
        // The off-thread sweep owns dirt-clearing; the viewport is painted
        // synchronously now and verified in place. The whole-doc synchronous
        // walk would race the pool.
        if let Some(mut pool) = self.hl_pool.take() {
            pool.reaim(&mut self.doc, rows);
            self.hl_pool = Some(pool);
        } else if let Some(pool) = HighlightPool::new(&self.doc, rows.clone()) {
            pool.speculate(&mut self.doc, rows);
            self.hl_pool = Some(pool);
        }
        true
    }

    /// One frame of the pool's sweep. A document that no longer uses the pool
    /// (shrunk below the threshold, or swapped to a grammar with no pool
    /// engine) deactivates any lingering one.
    pub(super) fn pool_sweep(&mut self) -> bool {
        if !self.uses_pool() {
            if let Some(pool) = &mut self.hl_pool {
                pool.active = false;
            }
            return false;
        }
        if let Some(mut pool) = self.hl_pool.take() {
            if pool.rev != self.doc.revision() {
                // An edit landed: re-sweep from a fresh snapshot AND repaint
                // the viewport now (the verified prefix in the cache survives).
                pool.restart(&mut self.doc, self.viewport.clone());
            } else {
                // Drain finished jobs and advance the verified chain.
                pool.poll(&mut self.doc);
            }
            let idle = !pool.active;
            self.hl_pool = Some(pool);
            // Once the sweep is idle, the synchronous phase-2 path refills any
            // window rows the sweep evicted (dirt is cleared, so this is a
            // cheap window refill, not O(doc)).
            if idle {
                let n = self.doc.buffer().line_count();
                self.doc.tokenize_highlight(n);
            }
        } else if let Some(pool) = HighlightPool::new(&self.doc, self.viewport.clone()) {
            // Large but no pool yet (grew past the threshold): create it rather
            // than tokenize the whole document synchronously.
            self.hl_pool = Some(pool);
        }
        true
    }

    /// Start the pool for a freshly loaded document or grammar, aimed at the
    /// current viewport (the first `ViewportChanged` reaims it). Recreating it
    /// here picks up a new grammar's engine.
    pub(super) fn pool_seed(&mut self) -> bool {
        let pooled = self.uses_pool();
        self.hl_pool = if pooled { HighlightPool::new(&self.doc, self.viewport.clone()) } else { None };
        pooled
    }

    /// Whether the pool still has work in flight.
    pub(super) fn pool_active(&self) -> bool {
        self.hl_pool.as_ref().is_some_and(|p| p.active)
    }
}

#[cfg(not(feature = "syntect"))]
impl CodeEditor {
    pub(super) fn pool_viewport(&mut self, _rows: std::ops::Range<u32>) -> bool {
        false
    }

    pub(super) fn pool_sweep(&mut self) -> bool {
        false
    }

    pub(super) fn pool_seed(&mut self) -> bool {
        false
    }

    pub(super) fn pool_active(&self) -> bool {
        false
    }
}

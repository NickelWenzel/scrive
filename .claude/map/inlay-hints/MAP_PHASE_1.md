# Phase 1 — scrive-core: the hint model and its anchored store

This is the implementation spec for Phase 1 of `MAP_PLAN.md` (Draft 7). It is self-contained: every
design rule this phase implements is restated below. Line numbers were read from source at HEAD
`8e72665` (branch `lsp_bridge`). Re-grep before editing if the tree has moved.

## 1. Prerequisites

- **No earlier phase.** This is the first phase of the inlay-hints plan. Base commit: `8e72665`.
- `git log --oneline -1` shows `8e72665` (or a descendant with no scrive-core changes), and
  `git status` shows only `.claude/`.
- **Baseline green** (the plan measured it at `8e72665`): 901 tests pass with `--all-features`;
  clippy (both feature sets), the doc build and the wasm all-features build are clean.
- Read in full before editing: `~/.claude/guides/RUST_STYLE.md`, `~/.claude/guides/OPAQUE.md`,
  the `/commit-and-comment` skill, `.claude/map/lsp-bridge/DISPATCH.md` (its rules apply with the
  map directory `.claude/map/inlay-hints/` and patches under `.claude/map/inlay-hints/patches/`),
  and these sources: `crates/scrive-core/src/{decorations.rs, document.rs, patch.rs, intel.rs,
  lib.rs, perf_gate.rs, perf.rs}`, `intel/ticket.rs`, `movement.rs:302-330`, `buffer.rs:270-310`,
  `sum_tree.rs:855-900` (`filter_visit`).
- Confirm the gaps this phase closes are still open:
  - `ls crates/scrive-core/src/intel/` has no `inlay.rs`;
  - `grep -n "InlayHint" crates/scrive-core/src/decorations.rs` prints nothing;
  - `DecorationKind` (decorations.rs:118-145) has the four variants `Diagnostic`, `FindMatch`,
    `SnippetStop`, `AutoClosePair`, and `empty_policy` (:152-157) has only `FindMatch` and `_`;
  - `Document` (document.rs:36-122) has no `inlays` field; `Views` (:2469-2479) has six fields.
- scrive-iced and scrive-lsp don't name `DecorationKind` anywhere (grepped), so the new variant
  touches no other crate.

## 2. Goal and exit criteria

**Goal.** scrive-core gains an LSP-free inlay-hint model and a dedicated, anchored hint store on
`Document`. A host installs a set of hints for one revision, and from then on each hint moves with
the token it annotates through the one mover (`rebase_views`), on edits, undo and redo, and goes
away with that token. Nothing is laid out or painted yet (Phases 3 and 4). With no hints installed,
every existing behaviour is unchanged.

**When this phase is done:**

1. `scrive_core::intel::inlay` exists with `Hint`, `Part`, `Padding`, `Kind`, `Placement`, `Side`,
   `Insert`, `Link`, `Key`, `Anchor`, `Placed`, `Shown`, `Outcome`, `Error`, plus
   `intel::inlay::request::Request` and `intel::inlay::interaction::{Interaction, Gesture}`.
2. `Hint::new` sanitises (control chars → `' '`), rejects a label with no visible text, and stores
   its width. Its placement defaults from the kind (`Type` → `Suffix`, `Parameter` → `Prefix`,
   `Other` → `Auto`) and `.placement(Placement)` overrides it (RESOLUTIONS R5).
   - `control_characters_in_a_label_become_spaces`
   - `a_label_without_visible_text_is_rejected`
   - `width_counts_padding_and_scalar_values`
   - `placement_defaults_from_the_kind_and_can_be_overridden` (boundary 2)
3. `DecorationKind::InlayHint(inlay::Anchor)` exists, with an explicit `empty_policy` arm. The
   mover drops an anchored hint whose whole token one edit replaces, on both movers, and the
   windowed/naive oracle holds with hint ranges in the store. `DecorationStore::visit_in`, the
   borrowing row query (R8), exists; `inlays_in` uses it here and Phase 3's row filter later.
   - `anchored_inlay_hints_drop_on_collapse_and_point_hints_keep`
   - `an_edit_replacing_a_whole_anchor_drops_the_hint_on_both_movers`
   - `windowed_apply_patch_equals_naive_with_inlay_hints`
   - `add_decoration_rejects_the_inlay_hint_kind` (debug builds)
   - `replace_all_mints_ids_in_item_order`
4. `Document` has `set_inlays`, `clear_inlays`, `remove_inlay`, `inlays_revision` and
   `inlays_in`, and the D3 anchoring table holds:
   - `typing_at_a_suffix_hint_lands_before_it`
   - `typing_at_a_prefix_hint_lands_after_it`
   - `enter_at_the_end_of_a_line_keeps_its_hint_on_that_line`
   - `enter_before_an_argument_moves_its_prefix_hint_to_the_first_non_blank`
   - `backspace_inside_the_anchor_word_keeps_the_hint`
   - `deleting_the_anchor_token_drops_the_hint_and_undo_does_not_restore_it`
   - `a_surviving_hint_rides_undo_and_redo`
   - `hints_without_a_neighbour_on_their_line_keep_a_zero_width_anchor`
   - `pasting_over_a_selection_drops_the_hints_anchored_there`
   - `a_line_replacing_edit_drops_the_hints_on_that_line`
   - `retyping_a_selected_word_drops_its_hint_on_either_side`
   - `moving_a_line_drops_the_hints_on_both_swapped_lines`
   - `auto_placement_follows_the_text_around_the_hint`
   - `mixed_sides_at_one_offset_render_as_suffixes_in_server_order`
   - `hints_render_in_offset_order_and_in_server_order_at_one_offset`
   - `a_stale_hint_set_is_refused_and_changes_nothing`
   - `clear_inlays_empties_the_set_and_forgets_its_revision`
   - `remove_inlay_takes_only_the_keyed_hint_at_its_render_offset`
   - `installed_hint_offsets_are_clipped_to_the_buffer`
   - `render_offset_clamps_each_side_to_its_own_row` (in `intel/inlay.rs`)
5. Perf gate: typing next to one of N hints is Constant and never re-sorts a store
   (`DECORATION_SORTS` delta 0); installing N hints is Linear.
   - `typing_next_to_a_hint_is_hint_count_independent`
   - `installing_hints_is_linear`
6. Every existing test stays green, unchanged. clippy (both feature sets), doc (`-D warnings`) and
   the wasm build are clean.

## 3. Design decisions implemented

Restated from MAP_PLAN.md. Where this doc adds detail the plan leaves open, it says
**Decision:**.

### D1 — the core hint model is LSP-free and display-sized

- New module `intel::inlay`. `Request` and `Interaction` live in their own submodules
  `intel/inlay/request.rs` and `intel/inlay/interaction.rs`, because scrive-lsp imports them on their
  own (RUST_STYLE: one semantic type per module; no `mod.rs`).
- `Hint` has private fields and is immutable once installed:
  `kind: Kind` (`Type | Parameter | Other`), `placement: Placement` (`Suffix | Prefix | Auto`),
  `label: Vec<Part>` (non-empty, each part's text sanitised: control chars → `' '`),
  `padding: Padding` (`{ left: bool, right: bool }`), `key: Key` (opaque to core, minted by the
  host), `insert: Insert` (`Available | Unavailable`), plus its stored `width`.
- `Part { text: String, link: Link }`, `Link::{Jumps, None}`, `Key(u64)`.
- Built by `Hint::new(kind, label, key) -> Result<Hint, inlay::Error>` plus builder setters
  `.padding(Padding)`, `.insert(Insert)` and `.placement(Placement)`. No raw `bool` parameters
  (OPAQUE N4).
- A hint carries **no offset**: an offset goes stale once the hint rides edits. Installing takes
  `Vec<inlay::Placed>`, where `Placed { offset: u32, hint: Hint }` (private fields,
  `Placed::new`) pairs a fetch-time offset with its payload. The store owns the position afterwards.
- `placement` (R5): `Hint::new` defaults it from the kind: `Type` → `Suffix`, `Parameter` →
  `Prefix`, `Other` → `Auto`. `.placement(Placement)` overrides it. Padding never changes it. The
  **client** computes an `Other` hint's placement from the **raw** server padding with Zed's rule
  (`hint_position_and_bias`, `project/src/lsp_command.rs:3898-3933` at zed `1399a80`: right-only →
  `Prefix`, left-only → `Suffix`, symmetric → `Auto`) before it collapses the padding (D16), and
  passes it with `.placement`. Core only resolves `Auto`.
- `set_inlays` resolves `Auto` against the buffer (RESOLUTIONS.md R27), using the core word
  classifier (`movement::is_word_char`, movement.rs:318): `Suffix` when a word char precedes `p`, or
  the char at `p` is whitespace, end of line/buffer, or one of `) ] } , ; .`; otherwise `Prefix`. `Auto` never reaches the store: the store keeps a resolved
  `Side { Suffix, Prefix }`, which has no `Auto`.
- Width in cells = `padding.left + Σ part chars + padding.right`, one cell per scalar value.
- A label with no visible text is rejected, so an installed hint always has width.
- Tooltips, locations and edits stay with the host. Core stores only what layout and gestures need.

**Getter names (R1).** The builder setters own the names `padding`, `insert` and `placement`,
so the read-only getters are `kind()`, `parts() -> &[Part]`, `padded() -> Padding`,
`insertable() -> bool`, `key() -> Key` and `width() -> u32` (stored at construction). `Part` has
`text() -> &str` and `link() -> Link`. There is no public placement getter: `Anchor::install`, in
the same module, reads the field.

**Decision: "no visible text"** means every part's sanitised text is whitespace (an empty label
vector qualifies). Empty parts inside a visible label are kept, because part indices must match the
server's label parts for later part-addressed gestures.

### D2 — hints live in a dedicated `DecorationStore`

- Wired like the auto-close store: a `Document` field, `Document::new`, `Views`, one line in
  `rebase_views`, and the `undo`/`redo` destructuring. Hints ride every edit, undo and redo through
  the one mover.
- New variant `DecorationKind::InlayHint(inlay::Anchor)`. `Anchor` is opaque with crate-private
  constructors. It holds `Arc<Hint>`, the resolved `Side`, the server index, and whether the range is
  anchored. So a host can't put a collapsed `Drop` range into the public bulk store through
  `decorations_mut()` (document.rs:1398).
- `empty_policy` gets an explicit `InlayHint` arm, not the `_` arm: `Drop` for an anchored range,
  `Keep` for a zero-width fallback.
- Diagnostics and find never see hints, and a hint publish never re-sorts them.

### D3 — a hint is anchored to the token it annotates

The anchor is the **word** next to `p` (the core word classifier), or one char when the neighbour
is not a word char, so backspacing a typo at the end of `count` doesn't drop its hint and jitter
the line.

| Side | Annotates | Stored range | Stickiness | Renders at | Typing at the hint's offset |
|---|---|---|---|---|---|
| `Suffix` | the token ending at `p` | `[word_start, p)` | `GrowsOnlyAfter` (R,R) | `min(range end, end of the row holding range start)` | text lands before the hint (`let xy: i32`) |
| `Prefix` | the token starting at `p` | `[p, word_end)` | `GrowsOnlyBefore` (L,L) | `max(range start, min(range end, first non-blank of the row holding range end))` | text lands after the hint (`foo(n: yx)`) |

- **Row clamp.** Enter at the end of a line grows a `Suffix` range across the newline and the
  auto-indent; the clamp keeps an end-of-line hint on its line. `Prefix` mirrors it for Enter typed
  before an argument.
- Deleting the whole anchor token empties the range, and `EmptyPolicy::Drop` removes the hint.
  **Undo does not restore a dropped hint**; the refetch the undo triggers does (a later phase).
- **Replacing the whole anchor drops the hint too.** Patch mapping keeps interior offsets
  (patch.rs:9-12, 201-214), so after a paste over a selection, a line-replacing edit or a host
  replacement, a surviving anchor would sit at an arbitrary byte inside the new text. Retyping a
  selected word would drop a `Suffix` hint (its `(R,R)` range collapses) but keep a `Prefix` one.
  So `remap_ranges` drops an anchored `InlayHint` range when one edit's old range covers the whole
  anchor and inserts text.
  - The coverage test reads the range in **old** coordinates, before `remap_ranges` overwrites
    `r.range` (decorations.rs:392-395).
  - It finds the covering edit with one `partition_point` on `old.end`, as `map_many` does
    (patch.rs:177), not O(n·e) comparisons.
  - A covering range touches the edit, so it is in the windowed mover's middle band
    (decorations.rs:883-899) and the windowed/naive equivalence holds.
  - Backspacing inside the word doesn't cover the whole anchor and keeps the hint.
  - `Document::edit` doesn't trim common prefixes, so `move_line` (Alt+↑/↓, document.rs:924-967,
    one op over both lines) and case changes drop the hints there until the refetch. Tested:
    Alt+↓ drops.
- **One owner for the render offset (R4):** `Anchor::render_offset(&self, range: Range<u32>,
  row_start: u32, row_end: u32, line: &str) -> Option<u32>` computes it from the stored range, the
  row's byte bounds and its line text (start in the row → `Suffix` renders here; end in the row →
  `Prefix` renders here), with no `offset_to_point` per hit; `None` means it renders on another
  row. `inlays_in` and `remove_inlay` use it here; Phase 3's row filter and `inlay_at` will too.
- **Fallback:** when there is no anchor on the same line (a `Suffix` hint at column 0; a `Prefix`
  hint at line end or EOF), the hint is a zero-width `Keep` range (not anchored) with
  `GrowsOnlyAfter` (`Suffix`) or `NeverGrows` (`Prefix`). No stored range is ever
  collapsed-and-`Drop`, so the windowed/naive equivalence holds.
- **No mid-word render guard** (R11): it would hide correct hints after typing at a shared offset
  (`||X -> fn()<…>f`). No phase adds one.

**Decision: the one-char anchor** is any char other than `\n` (whitespace included).

### D4 — replace, don't merge

- `Document::set_inlays(revision, Vec<inlay::Placed>) -> inlay::Outcome` replaces the whole store
  when `revision == doc.revision()`, else returns `Stale` and changes nothing (the
  `set_diagnostics` shape). `clear_inlays()` empties it. `inlays_revision() -> Option<Revision>` is
  the revision the set was installed at (D14 compares it with `revision()` to gate gestures).
  `remove_inlay(key, offset)` removes one hint by key at its current render offset; it is called at
  the current revision, so the offset is exact (D19).
- **Mixed sides at one fetch offset are normalised to `Suffix`.** The LSP spec says hints at one
  position "are shown in the order they appear in the response", and no single caret split keeps
  `L ‹suffix›‹prefix› R`. So `set_inlays` groups by fetch offset; if a group has both sides, every
  hint in it is anchored as `Suffix` (Zed's `normalize_hint_biases`; its test
  `test_colocated_mixed_kind_hints_share_bias`, inlay_hints.rs:5764-5821).
- **Server order is kept.** Ids are minted in server order before grouping and normalisation, and
  `Anchor` stores the server index. The store orders by `(start, id)` (decorations.rs:985-991), which
  is not render order (a `Suffix` range starts at its word start), so readers sort by
  `(render offset, Prefix before Suffix, server index)`.
- **Anchors never leave the inlay store.** No public API returns an `InlayHint(Anchor)`:
  `inlays_in` returns a dedicated `inlay::Shown` view. `add_decoration`, `add_sorted_batch` and
  `splice_sorted_batch` `debug_assert!` against the `InlayHint` kind (no early return: it is
  unreachable from outside, and returning would need a fake `DecorationId`).

**Decisions this doc makes where the plan is silent:**

- Install clips each offset to the buffer and snaps it left to a char boundary
  (`Buffer::clip_offset(offset, Bias::Left)`), before `Auto` resolution and grouping.
- `Outcome::Stale { current: Revision }` (typed), not the `u64` of `DiagnosticsOutcome`.
- `clear_inlays` also resets `inlays_revision()` to `None`.
- `inlays_in(range)` yields the hints whose render offset lies in `range.start..=range.end`
  (touching, as `decorations_in` does), sorted by `(offset, Prefix first, server index)`. It walks
  the rows `range` spans, one windowed store query per row.
- `remove_inlay` returns whether it removed a hint.
- The store gains three crate-private methods: `replace_all` (mints ids in item order, one sort),
  `clear` (keeps the id counter) and `visit_in` (R8: the borrowing row query D5 asks for, which
  yields each touching range with its kind borrowed for the store's lifetime; `inlays_in` uses it,
  and Phase 3's row filter keeps the borrow in its spans). `SumTree::filter_visit` ties the visited
  item's borrow to `&self` so `visit_in` can hand it out.
- The interaction type's shape (R6): `Interaction { ticket, key, gesture }` with
  `interaction::Gesture::{Tooltip { part: u32 }, Jump { part: u32 }, Insert { offset: u32 }}`.
  A tooltip always has a part, because padding is not hoverable.
- The windowed/naive oracle grows its hint case as a **new** test, so the existing oracle stays
  byte-for-byte as it is.

### Constraints that bite here

- RUST_STYLE: no `mod.rs`; module paths over composite names (`inlay::Hint`, not `InlayHint` as a
  type); no aliased imports; typed errors (`thiserror` is already a dependency); `expect` over
  `unwrap` in library code; tests in-file with sentence names.
- OPAQUE: `Hint`, `Part`, `Key`, `Placed`, `Anchor`, `Shown`, `Request` and `Interaction` have
  private fields. `Padding` is plain data with public fields (no invariant; a struct literal reads
  better than two `bool` parameters).
- scrive-core stays headless: no lsp-types, no I/O, no clocks.
- Performance: a single edit moves hints in `O(window + log n)`; no store sort per keystroke;
  hints never enter the `FoldMap` or its cache key.
- `#![deny(missing_docs)]`. Comments follow DISPATCH override 1: doc comments say what and the
  contract; other comments only a non-obvious why; never "Phase N", "D3" or plan history. Treat the
  doc comments below as content hints and trim them to that rule.
- The code is not rustfmt-clean. Never run `cargo fmt`; `rustfmt --edition 2021` only on the three
  new files.

## 4. Step-by-step changes

Commit boundaries (DISPATCH override 2: you never commit; at each boundary make the tree green,
save `.claude/map/inlay-hints/patches/phase1-<k>.patch` against `8e72665` with `git add -N` on new
files first, and write `phase1-<k>.msg`):

| k | Subject | Contents |
|---|---|---|
| 1 | `feat(core): inlay hint model` | Steps 1–3 |
| 2 | `feat(core): anchor inlay hints to the tokens they annotate` | Steps 4–6 |
| 3 | `test(core): gate inlay hint install and typing costs` | Step 7 |

Suggested "why" bodies:
1. "Hosts need an LSP-free hint type before the document can anchor, lay out or interact with
   hints."
2. "Hints ride edits like diagnostics, but drop with their token, so a deleted argument never
   leaves a stale label behind."
3. "Installing scales with the set and typing next to a hint stays flat; pin both before layout
   starts querying the store."

Boundary 1 can't include `Anchor`, `Shown` or `Placed::into_parts`: their crate-private methods
would be dead code without their callers (`-D warnings`; DISPATCH forbids `#[allow(dead_code)]`).
The same holds for `Hint`'s `placement` field: no public getter reads it (R1), so it lands in
boundary 2 with its setter and its only reader, `Anchor::install`. `visit_in` and the
`filter_visit` lifetime land in boundary 2 with `inlays_in`.

### Step 1 — `crates/scrive-core/src/intel.rs`

Current (intel.rs:1-2 and :16-25):

```rust
//! Language services — completion, signature help, hover, and the
//! goto-definition, rename and format commands.
...
pub mod completion;
pub mod definition;
pub mod format;
pub mod hover;
pub mod providers;
...
```

New: first doc line names inlay hints, and the module list gains `inlay` in order:

```rust
//! Language services — completion, signature help, hover, inlay hints, and
//! the goto-definition, rename and format commands.
...
pub mod completion;
pub mod definition;
pub mod format;
pub mod hover;
pub mod inlay;
pub mod providers;
...
```

Leave the rest of the module doc alone.

### Step 2 — `crates/scrive-core/src/intel/inlay.rs` (new), boundary-1 part

```rust
//! Inlay hints: short labels a language service places between buffer
//! characters, such as `: i32` after a binding or `name:` before an argument.
//! They are not buffer text. A host builds [`Hint`]s, pairs each with the
//! offset it was computed for ([`Placed`]), and installs the set with
//! [`Document::set_inlays`](crate::Document::set_inlays). From then on the
//! document moves every hint with the token it annotates.
//!
//! The model holds only what layout and gestures need. Tooltips, locations and
//! edits stay with the host, which finds them again by [`Key`].

pub mod interaction;
pub mod request;

pub use interaction::Interaction;
pub use request::Request;

use crate::buffer::Revision;

/// What a hint annotates, which decides the neighbour it sticks to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A type annotation, following the token before it (`x: i32`).
    Type,
    /// A parameter name, preceding the argument after it (`n: 5`).
    Parameter,
    /// Anything else: the host's placement, and failing that the text
    /// around it, decide its side.
    Other,
}

/// Which neighbour a hint annotates, before the document has seen the text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
    /// The token ending at the hint's offset.
    Suffix,
    /// The token starting at the hint's offset.
    Prefix,
    /// Decided against the text at install: [`Side::Prefix`] when a word
    /// starts at the offset and no word character precedes it, else
    /// [`Side::Suffix`].
    Auto,
}

/// Which neighbour an installed hint annotates. Text typed at a
/// [`Suffix`](Self::Suffix) hint's offset lands before it; at a
/// [`Prefix`](Self::Prefix) hint's offset, after it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// Annotates the token ending at its offset.
    Suffix,
    /// Annotates the token starting at its offset.
    Prefix,
}

/// Blank cells on either side of a label. Padding is editor background, not
/// part of the label.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Padding {
    /// One blank cell before the label.
    pub left: bool,
    /// One blank cell after the label.
    pub right: bool,
}

/// Whether a label part leads somewhere when activated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    /// The host holds a location for this part.
    Jumps,
    /// The part is plain text.
    None,
}

/// Whether the host can turn a hint into buffer text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Insert {
    /// The host holds edits that insert this hint.
    Available,
    /// The hint is display-only.
    Unavailable,
}

/// A hint's identity, minted by the host and opaque to the core. The host
/// finds a hint's tooltip, locations and edits again by its key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key(u64);

/// One piece of a hint's label. Its text never holds a control character.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Part {
    text: String,
    link: Link,
}

/// One inlay hint: what it annotates, its label, and its key. Immutable once
/// installed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hint {
    kind: Kind,
    label: Vec<Part>,
    padding: Padding,
    key: Key,
    insert: Insert,
    width: u32,
}

/// A hint paired with the byte offset it was computed for. This is what a host
/// hands to [`Document::set_inlays`](crate::Document::set_inlays).
#[derive(Clone, Debug)]
pub struct Placed {
    offset: u32,
    hint: Hint,
}

/// The result of installing a hint set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The set was current and replaced the previous one.
    Applied {
        /// How many hints were installed.
        count: usize,
    },
    /// The set was computed for another revision; nothing changed.
    Stale {
        /// The document's revision when the set was refused.
        current: Revision,
    },
}

/// Why a hint could not be built.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Error {
    /// Every part of the label is empty or whitespace.
    #[error("an inlay hint label needs visible text")]
    EmptyLabel,
}

impl Key {
    /// The key with the host's raw identity `raw`.
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }
}

impl Part {
    /// A label part. Control characters in `text` (tabs and newlines included)
    /// become spaces, so every char occupies exactly one cell.
    #[must_use]
    pub fn new(text: impl Into<String>, link: Link) -> Self {
        let text = text.into().chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        Self { text, link }
    }

    /// The part's text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the part leads somewhere.
    #[must_use]
    pub fn link(&self) -> Link {
        self.link
    }
}

impl Hint {
    /// A hint of `kind` labelled `label`, unpadded and display-only.
    ///
    /// # Errors
    ///
    /// [`Error::EmptyLabel`] when no part has visible text.
    pub fn new(kind: Kind, label: Vec<Part>, key: Key) -> Result<Self, Error> {
        if label.iter().all(|part| part.text.trim().is_empty()) {
            return Err(Error::EmptyLabel);
        }
        let padding = Padding::default();
        Ok(Self {
            kind,
            width: width(&label, padding),
            label,
            padding,
            key,
            insert: Insert::Unavailable,
        })
    }

    /// The same hint with `padding`, which counts towards the width.
    #[must_use]
    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self.width = width(&self.label, padding);
        self
    }

    /// The same hint with `insert`.
    #[must_use]
    pub fn insert(mut self, insert: Insert) -> Self {
        self.insert = insert;
        self
    }

    /// What the hint annotates.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The label's parts, in display order.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.label
    }

    /// The blank cells around the label.
    #[must_use]
    pub fn padded(&self) -> Padding {
        self.padding
    }

    /// The host's key for this hint.
    #[must_use]
    pub fn key(&self) -> Key {
        self.key
    }

    /// Whether the host can insert this hint as text.
    #[must_use]
    pub fn insertable(&self) -> bool {
        self.insert == Insert::Available
    }

    /// Cells the hint occupies: its padding plus one cell per char of its
    /// label.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }
}

impl Placed {
    /// `hint`, computed for byte `offset` of the revision it will be
    /// installed at.
    #[must_use]
    pub fn new(offset: u32, hint: Hint) -> Self {
        Self { offset, hint }
    }

    /// The offset the hint was computed for.
    #[must_use]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// The hint.
    #[must_use]
    pub fn hint(&self) -> &Hint {
        &self.hint
    }
}

fn width(label: &[Part], padding: Padding) -> u32 {
    let text: u32 = label.iter().map(|part| part.text.chars().count() as u32).sum();
    u32::from(padding.left) + text + u32::from(padding.right)
}
```

Tests at the bottom of the file (boundary 1):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn label(text: &str) -> Vec<Part> {
        vec![Part::new(text, Link::None)]
    }

    /// A tab or newline in a label would break the one-cell-per-char grid,
    /// so every control character becomes a space.
    #[test]
    fn control_characters_in_a_label_become_spaces() {
        assert_eq!(Part::new("a\tb\nc\u{7}", Link::None).text(), "a b c ", "control chars are spaces");
    }

    /// An installed hint always has visible text, so it always has width.
    #[test]
    fn a_label_without_visible_text_is_rejected() {
        for parts in [vec![], label(""), label(" \t"), vec![Part::new("", Link::None), Part::new("\n", Link::None)]] {
            assert_eq!(Hint::new(Kind::Type, parts, Key::new(1)), Err(Error::EmptyLabel), "no visible text");
        }
        let kept = Hint::new(Kind::Type, vec![Part::new("", Link::None), Part::new("x", Link::None)], Key::new(1))
            .expect("one visible part is enough");
        assert_eq!(kept.parts().len(), 2, "empty parts keep their index");
    }

    /// Width is padding plus one cell per scalar value, whatever its byte length.
    #[test]
    fn width_counts_padding_and_scalar_values() {
        let hint = Hint::new(Kind::Other, vec![Part::new("ab", Link::None), Part::new("é😀", Link::Jumps)], Key::new(1))
            .expect("visible");
        assert_eq!(hint.width(), 4, "two parts, four chars");
        assert_eq!(hint.padding(Padding { left: true, right: true }).width(), 6, "padding adds a cell each side");
    }
}
```

The placement test lands in boundary 2 with the field it reads (Step 4).

### Step 3 — `intel/inlay/request.rs` and `intel/inlay/interaction.rs` (new)

`crates/scrive-core/src/intel/inlay/request.rs`:

```rust
//! The fetch an editor records when its inlay hints are due: which bytes to
//! cover, under which ticket.

use std::ops::Range;

use crate::intel::ticket::Ticket;

/// A request for the hints in a byte span. The answer lands only under its
/// ticket, at the ticket's revision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    ticket: Ticket,
    span: Range<u32>,
}

impl Request {
    /// A request for the hints in `span`, made under `ticket`.
    #[must_use]
    pub fn new(ticket: Ticket, span: Range<u32>) -> Self {
        Self { ticket, span }
    }

    /// The ticket the answer must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The byte span to cover, at the ticket's revision.
    #[must_use]
    pub fn span(&self) -> Range<u32> {
        self.span.clone()
    }
}
```

`crates/scrive-core/src/intel/inlay/interaction.rs`:

```rust
//! A gesture on an installed hint that the host must answer: show a tooltip,
//! follow a label part's location, or insert the hint as text.

use crate::intel::inlay::Key;
use crate::intel::ticket::Ticket;

/// One gesture on the hint keyed `key`, recorded under `ticket`. The host
/// answers it only while its hint set is still the one at the ticket's
/// revision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interaction {
    ticket: Ticket,
    key: Key,
    gesture: Gesture,
}

/// What the user did to the hint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gesture {
    /// The pointer rests on label part `part` of the hint.
    Tooltip {
        /// The hovered label part's index.
        part: u32,
    },
    /// Follow label part `part`'s location.
    Jump {
        /// The clicked label part's index.
        part: u32,
    },
    /// Insert the hint, which renders at `offset`, as buffer text.
    Insert {
        /// The hint's render offset.
        offset: u32,
    },
}

impl Interaction {
    /// Ask for the tooltip of label part `part` of hint `key` (the host falls
    /// back to the hint's own tooltip).
    #[must_use]
    pub fn tooltip(ticket: Ticket, key: Key, part: u32) -> Self {
        Self { ticket, key, gesture: Gesture::Tooltip { part } }
    }

    /// Follow label part `part` of hint `key`.
    #[must_use]
    pub fn jump(ticket: Ticket, key: Key, part: u32) -> Self {
        Self { ticket, key, gesture: Gesture::Jump { part } }
    }

    /// Insert hint `key`, which renders at `offset`.
    #[must_use]
    pub fn insert(ticket: Ticket, key: Key, offset: u32) -> Self {
        Self { ticket, key, gesture: Gesture::Insert { offset } }
    }

    /// The ticket the answer must carry.
    #[must_use]
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }

    /// The hint the gesture targets.
    #[must_use]
    pub fn key(&self) -> Key {
        self.key
    }

    /// What the user did.
    #[must_use]
    pub fn gesture(&self) -> Gesture {
        self.gesture
    }
}
```

These two modules have nothing to test beyond their constructors. If you add tests, one per file
is enough (e.g. `each_gesture_keeps_its_ticket_and_key`, minting tickets with
`crate::intel::ticket::Counter::new().issue(Revision(0))`).

`crates/scrive-core/src/lib.rs:26-27`, current:

```rust
//! - attach diagnostics / snippet stops → [`decorations`]
//! - complete / hover / signature help → [`intel::providers`]
```

New (one line appended):

```rust
//! - attach diagnostics / snippet stops → [`decorations`]
//! - complete / hover / signature help → [`intel::providers`]
//! - show inlay hints → [`intel::inlay`]
```

No crate-root `pub use` for the inlay types: they are reached by module path
(`scrive_core::intel::inlay::Hint`). Inside `inlay.rs`, the two `pub use` lines in Step 2 make
`inlay::Request` and `inlay::Interaction` the paths the plan and Phases 5–7 write (D11, D12);
`inlay::interaction::Gesture` stays in its module. **Boundary 1 ends here.**

### Step 4 — `intel/inlay.rs`, boundary-2 part: placement, `Anchor`, `Shown`, `Placed::into_parts`

**The placement (R5).** `Hint` gains its `placement` field (after `kind`), `Hint::new` sets
`placement: default_placement(kind),`, and the builder gets the setter. There is no getter
(R1); `Anchor::install` reads the field in this module.

```rust
impl Hint {
    // ... Step 2's methods ...

    /// The same hint, annotating the neighbour `placement` names instead of
    /// the one its kind implies.
    #[must_use]
    pub fn placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }
}

/// The neighbour a hint of `kind` annotates unless its host says otherwise.
fn default_placement(kind: Kind) -> Placement {
    match kind {
        Kind::Type => Placement::Suffix,
        Kind::Parameter => Placement::Prefix,
        Kind::Other => Placement::Auto,
    }
}
```

Imports become:

```rust
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use crate::buffer::{Buffer, Revision};
use crate::coords::Bias;
use crate::decorations::{EmptyPolicy, Stickiness};
use crate::movement::is_word_char;
```

New types, after `Placed` (they must be `pub`: `Anchor` appears in the public
`DecorationKind::InlayHint` variant, `Shown` is what `inlays_in` yields):

```rust
/// An installed hint's place in the document's inlay store: the hint, the
/// side it annotates, and its position in the set it arrived in. Only the
/// store creates one, and none ever leaves it.
#[derive(Clone, Debug)]
pub struct Anchor {
    hint: Arc<Hint>,
    side: Side,
    anchored: bool,
    index: u32,
}

/// One hint as it renders: where, on which side of that offset, and what.
#[derive(Clone, Debug)]
pub struct Shown {
    offset: u32,
    side: Side,
    index: u32,
    hint: Arc<Hint>,
}
```

`anchored` is a private field, never a parameter: the two constructors name the two cases.

```rust
impl Placed {
    // ... `new`, `offset`, `hint` as in Step 2 ...

    pub(crate) fn into_parts(self) -> (u32, Hint) {
        (self.offset, self.hint)
    }
}

impl Anchor {
    /// Anchor every hint in `placed` to the token it annotates in `buffer`:
    /// each hint's range, anchor and stickiness for the inlay store, in
    /// `placed` order.
    pub(crate) fn install(buffer: &Buffer, placed: Vec<Placed>) -> Vec<(Range<u32>, Self, Stickiness)> {
        let wanted: Vec<(u32, Side, Hint)> = placed
            .into_iter()
            .map(|placed| {
                let (offset, hint) = placed.into_parts();
                let offset = buffer.clip_offset(offset, Bias::Left);
                let side = match hint.placement {
                    Placement::Suffix => Side::Suffix,
                    Placement::Prefix => Side::Prefix,
                    Placement::Auto => auto_side(buffer, offset),
                };
                (offset, side, hint)
            })
            .collect();
        let mixed = mixed_offsets(&wanted);
        wanted
            .into_iter()
            .enumerate()
            .map(|(index, (offset, side, hint))| {
                // The LSP spec shows hints at one position in response order,
                // and only a shared side keeps that order next to the caret.
                let side = if mixed.contains(&offset) { Side::Suffix } else { side };
                place(buffer, offset, side, Arc::new(hint), index as u32)
            })
            .collect()
    }

    /// An anchor whose range covers the token its hint annotates.
    pub(crate) fn token(hint: Arc<Hint>, side: Side, index: u32) -> Self {
        Self { hint, side, anchored: true, index }
    }

    /// A zero-width anchor, for a hint with no token beside it on its line.
    pub(crate) fn point(hint: Arc<Hint>, side: Side, index: u32) -> Self {
        Self { hint, side, anchored: false, index }
    }

    pub(crate) fn hint(&self) -> &Hint {
        &self.hint
    }

    pub(crate) fn is_anchored(&self) -> bool {
        self.anchored
    }

    /// A hint anchored to a token goes with it; a zero-width one stays until
    /// the next set replaces it.
    pub(crate) fn empty_policy(&self) -> EmptyPolicy {
        if self.anchored { EmptyPolicy::Drop } else { EmptyPolicy::Keep }
    }

    /// Where the hint renders on the row spanning bytes `row_start..=row_end`
    /// with text `line` (no newline), given its stored `range`, or `None`
    /// when it renders on another row. A suffix renders on the row holding
    /// its range start, a prefix on the row holding its range end, so each
    /// hint renders on exactly one row.
    pub(crate) fn render_offset(&self, range: Range<u32>, row_start: u32, row_end: u32, line: &str) -> Option<u32> {
        debug_assert_eq!(row_end - row_start, line.len() as u32, "the row's bounds are its line's");
        let on_row = |offset: u32| (row_start..=row_end).contains(&offset);
        match self.side {
            Side::Suffix => on_row(range.start).then(|| range.end.min(row_end)),
            Side::Prefix => on_row(range.end).then(|| {
                let first_non_blank = row_end - line.trim_start().len() as u32;
                range.start.max(range.end.min(first_non_blank))
            }),
        }
    }

    /// This hint as rendered at `offset`.
    pub(crate) fn shown(&self, offset: u32) -> Shown {
        Shown { offset, side: self.side, index: self.index, hint: Arc::clone(&self.hint) }
    }
}

impl Shown {
    /// The hint's key.
    #[must_use]
    pub fn key(&self) -> Key {
        self.hint.key
    }

    /// The byte offset the hint renders at.
    #[must_use]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// Which side of [`offset`](Self::offset) the hint annotates.
    #[must_use]
    pub fn side(&self) -> Side {
        self.side
    }

    /// The hint.
    #[must_use]
    pub fn hint(&self) -> &Hint {
        &self.hint
    }

    /// Render order: by offset; at one offset prefixes before suffixes, each
    /// in the order the set arrived in.
    pub(crate) fn render_order(&self) -> (u32, bool, u32) {
        (self.offset, self.side == Side::Suffix, self.index)
    }
}
```

Private helpers at the bottom of the non-test code, next to `placement` and `width`:

```rust
/// `Auto`'s rule: a suffix after a word or before a closer, separator or line end; else a prefix.
fn auto_side(buffer: &Buffer, offset: u32) -> Side {
    let word_before = buffer.char_before(offset).is_some_and(is_word_char);
    let closes = buffer
        .char_at(offset)
        .is_none_or(|c| c.is_whitespace() || matches!(c, ')' | ']' | '}' | ',' | ';' | '.'));
    if word_before || closes { Side::Suffix } else { Side::Prefix }
}

/// The fetch offsets where hints want both sides.
fn mixed_offsets(wanted: &[(u32, Side, Hint)]) -> HashSet<u32> {
    let mut first: HashMap<u32, Side> = HashMap::new();
    let mut mixed = HashSet::new();
    for &(offset, side, _) in wanted {
        if *first.entry(offset).or_insert(side) != side {
            mixed.insert(offset);
        }
    }
    mixed
}

fn place(buffer: &Buffer, offset: u32, side: Side, hint: Arc<Hint>, index: u32) -> (Range<u32>, Anchor, Stickiness) {
    match side {
        Side::Suffix => match token_ending_at(buffer, offset) {
            Some(start) => (start..offset, Anchor::token(hint, side, index), Stickiness::GrowsOnlyAfter),
            None => (offset..offset, Anchor::point(hint, side, index), Stickiness::GrowsOnlyAfter),
        },
        Side::Prefix => match token_starting_at(buffer, offset) {
            Some(end) => (offset..end, Anchor::token(hint, side, index), Stickiness::GrowsOnlyBefore),
            None => (offset..offset, Anchor::point(hint, side, index), Stickiness::NeverGrows),
        },
    }
}

/// Where the token ending at `offset` starts: the word there, else the one
/// char before it. `None` at a line or document start.
fn token_ending_at(buffer: &Buffer, offset: u32) -> Option<u32> {
    let mut start = offset;
    while let Some(c) = buffer.char_before(start).filter(|&c| is_word_char(c)) {
        start -= c.len_utf8() as u32;
    }
    if start < offset {
        return Some(start);
    }
    buffer.char_before(offset).filter(|&c| c != '\n').map(|c| offset - c.len_utf8() as u32)
}

/// Where the token starting at `offset` ends: the word there, else the one
/// char after it. `None` at a line or document end.
fn token_starting_at(buffer: &Buffer, offset: u32) -> Option<u32> {
    let mut end = offset;
    while let Some(c) = buffer.char_at(end).filter(|&c| is_word_char(c)) {
        end += c.len_utf8() as u32;
    }
    if end > offset {
        return Some(end);
    }
    buffer.char_at(offset).filter(|&c| c != '\n').map(|c| offset + c.len_utf8() as u32)
}
```

`is_word_char` never accepts `\n`, so the word scans stay on their line. `char_at`/`char_before`
need char-boundary offsets; install clipped them, and every step moves by `len_utf8`.

Add one test to the file's `mod tests` (boundary 2):

```rust
    /// A suffix renders at its range end, clamped to the row its range starts
    /// on; a prefix at the first non-blank of the row its range ends on; and
    /// neither renders on any other row.
    #[test]
    fn render_offset_clamps_each_side_to_its_own_row() {
        let hint = || Arc::new(Hint::new(Kind::Other, label("h"), Key::new(1)).expect("visible"));
        // "    foo()\n    " after Enter at the end of row 0: the suffix range grew over the newline.
        let suffix = Anchor::token(hint(), Side::Suffix, 0);
        assert_eq!(suffix.render_offset(8..14, 0, 9, "    foo()"), Some(9), "clamped to the end of its start row");
        assert_eq!(suffix.render_offset(8..14, 10, 14, "    "), None, "not on the row its range ends on");
        // "    foo(\n    a)" after Enter before `a`: the prefix range starts on row 0.
        let prefix = Anchor::token(hint(), Side::Prefix, 0);
        assert_eq!(prefix.render_offset(8..14, 9, 15, "    a)"), Some(13), "at the first non-blank of its end row");
        assert_eq!(prefix.render_offset(8..14, 0, 8, "    foo("), None, "not on the row its range starts on");
        assert_eq!(prefix.render_offset(8..9, 0, 10, "    foo(a)"), Some(8), "on one row it renders at its start");
    }

    /// Type and parameter hints default to their fixed sides and other hints
    /// to `Auto`; padding never changes that, and `.placement` overrides it.
    #[test]
    fn placement_defaults_from_the_kind_and_can_be_overridden() {
        let hint = |kind| Hint::new(kind, label("h"), Key::new(1)).expect("visible");
        assert_eq!(hint(Kind::Type).placement, Placement::Suffix, "type hints follow their token");
        assert_eq!(hint(Kind::Parameter).placement, Placement::Prefix, "parameter hints precede theirs");
        assert_eq!(hint(Kind::Other).placement, Placement::Auto, "other hints wait for the text");
        let padded = hint(Kind::Other).padding(Padding { left: false, right: true });
        assert_eq!(padded.placement, Placement::Auto, "padding is the host's to read, not core's");
        let placed = hint(Kind::Other).placement(Placement::Prefix);
        assert_eq!(placed.placement, Placement::Prefix, "the host's placement wins");
    }
```

### Step 5 — `crates/scrive-core/src/decorations.rs`

5a. Imports, decorations.rs:29-34. Add `use crate::intel::inlay;` after `use crate::coords::Bias;`.

5b. The variant, inside `DecorationKind` (decorations.rs:118-145), after `AutoClosePair`:

```rust
    /// The provenance region of an auto-inserted closing pair.
    AutoClosePair,
    /// An inlay hint, ranged over the token it annotates. Only a document's
    /// inlay store holds this kind, and no public API hands one out. An
    /// anchored hint goes with its token: when the token is deleted, or one
    /// edit replaces all of it.
    InlayHint(inlay::Anchor),
}
```

5c. `empty_policy`, decorations.rs:147-158. Current:

```rust
    /// Post-commit policy for a range of this kind that has collapsed to empty:
    /// [`FindMatch`](Self::FindMatch) is re-queried so it drops; everything else
    /// keeps (the owner controls its lifetime).
    #[must_use]
    pub fn empty_policy(&self) -> EmptyPolicy {
        match self {
            Self::FindMatch => EmptyPolicy::Drop,
            _ => EmptyPolicy::Keep,
        }
    }
```

New:

```rust
    /// Post-commit policy for a range of this kind that has collapsed to empty:
    /// [`FindMatch`](Self::FindMatch) is re-queried so it drops, and an
    /// [`InlayHint`](Self::InlayHint) anchored to a token goes with the token;
    /// everything else keeps (the owner controls its lifetime).
    #[must_use]
    pub fn empty_policy(&self) -> EmptyPolicy {
        match self {
            Self::FindMatch => EmptyPolicy::Drop,
            Self::InlayHint(anchor) => anchor.empty_policy(),
            _ => EmptyPolicy::Keep,
        }
    }
```

`DecoItem::summary` (:293-304) and `first_start_with_severity` (:728-731) already have `_` arms;
`find_count` uses `matches!`. They need no change.

5d. `remap_ranges`, decorations.rs:378-400. Current:

```rust
fn remap_ranges(patch: &Patch, v: &mut Vec<TrackedRange>) {
    let mut queries: Vec<(u32, Bias)> = Vec::with_capacity(v.len() * 2);
    for r in v.iter() {
        let (bs, be) = r.stickiness.biases();
        queries.push((r.range.start, bs));
        queries.push((r.range.end, be));
    }
    let mut mapped: Vec<u32> = Vec::new();
    patch.map_many(&queries, &mut mapped);
    for (i, r) in v.iter_mut().enumerate() {
        let (ms, me) = (mapped[2 * i], mapped[2 * i + 1]);
        r.range = ms.min(me)..me;
    }
    v.retain(|r| {
        let collapsed = r.range.start == r.range.end;
        !(collapsed && matches!(r.kind.empty_policy(), EmptyPolicy::Drop))
    });
}
```

New (doc comment extended by one sentence: "…and drop the inlay hints whose whole token one edit
replaced."):

```rust
fn remap_ranges(patch: &Patch, v: &mut Vec<TrackedRange>) {
    let mut queries: Vec<(u32, Bias)> = Vec::with_capacity(v.len() * 2);
    // Coverage is a question about the old text, so read it before the remap
    // below overwrites the ranges.
    let mut replaced: Vec<bool> = Vec::with_capacity(v.len());
    for r in v.iter() {
        let (bs, be) = r.stickiness.biases();
        queries.push((r.range.start, bs));
        queries.push((r.range.end, be));
        replaced.push(rides_a_token(&r.kind) && replaces_whole(patch.edits(), &r.range));
    }
    let mut mapped: Vec<u32> = Vec::new();
    patch.map_many(&queries, &mut mapped);
    for (i, r) in v.iter_mut().enumerate() {
        let (ms, me) = (mapped[2 * i], mapped[2 * i + 1]);
        r.range = ms.min(me)..me;
    }
    let mut replaced = replaced.into_iter();
    v.retain(|r| {
        let replaced = replaced.next().expect("one flag per range");
        let collapsed = r.range.start == r.range.end;
        !(replaced || (collapsed && matches!(r.kind.empty_policy(), EmptyPolicy::Drop)))
    });
}

/// Whether `kind` is an inlay hint anchored to a token.
fn rides_a_token(kind: &DecorationKind) -> bool {
    matches!(kind, DecorationKind::InlayHint(anchor) if anchor.is_anchored())
}

/// Whether one of `edits` (ascending, disjoint) replaces all of `range` with
/// new text. Only the first edit whose old end reaches `range.end` can cover
/// it: every later edit starts at or after that end.
fn replaces_whole(edits: &[crate::patch::Edit], range: &Range<u32>) -> bool {
    let k = edits.partition_point(|e| e.old.end < range.end);
    edits.get(k).is_some_and(|e| e.old.start <= range.start && e.new.start < e.new.end)
}
```

`Vec::retain` visits every element once, in order, which is what pairs each flag with its range.
A covering pure deletion has `new` empty and is caught by the collapse rule instead, so the
`new.start < new.end` test changes nothing for it.

5e. The windowed mover's doc (decorations.rs:883-887) says the middle band is complete for the
collapse rule. Append one sentence: "The same holds for an inlay hint whose whole token the edit
replaces: covering the token means touching the edit." No code change in `apply_single_edit`.

5f. Bulk inserts reject the kind. In `add_decoration` (:477-488), `splice_sorted_batch`
(:628-669) and `add_sorted_batch` (:746-763), first statement of each body:

```rust
        debug_assert!(
            !matches!(kind, DecorationKind::InlayHint(_)),
            "inlay hints enter a store only through `replace_all`",
        );
```

5g. Two crate-private store methods. Put them after `set_diagnostics` (:936-965), before `mint`:

```rust
    /// Replace every range with `items`, minting ids in item order so that, at
    /// one start, the items keep that order. One sort for the whole set.
    pub(crate) fn replace_all(&mut self, items: impl IntoIterator<Item = (Range<u32>, DecorationKind, Stickiness)>) {
        let v: Vec<TrackedRange> = items
            .into_iter()
            .map(|(range, kind, stickiness)| TrackedRange { id: self.mint(), range, kind, stickiness })
            .collect();
        self.set_sorted(v);
    }

    /// Remove every range. Ids already minted are never reused.
    pub(crate) fn clear(&mut self) {
        self.tree = SumTree::new();
    }
```

`set_sorted` (:985-991) counts one `DECORATION_SORTS` and charges `len` to the meter, which is what
makes install Linear.

5g′. **The borrowing row query (R8).** D5 asks for a per-row query that borrows instead of
cloning every hit's kind (`decorations_in`, decorations.rs:549-581, clones an `Arc` per hit).
`SumTree::filter_visit` (sum_tree.rs:861) takes `G: FnMut(&T, &D)`, a higher-ranked closure, so
nothing it visits can outlive the call. Tie the item borrow to `&self`:

```rust
// before
pub fn filter_visit<D: Dimension<T::Summary>, F: Fn(&D, &T::Summary) -> bool, G: FnMut(&T, &D)>(
    &self,
    descend: &F,
    visit: &mut G,
) {
// after (filter_visit_from, sum_tree.rs:869, changes the same way)
pub fn filter_visit<'s, D: Dimension<T::Summary>, F: Fn(&D, &T::Summary) -> bool, G: FnMut(&'s T, &D)>(
    &'s self,
    descend: &F,
    visit: &mut G,
) {
```

Every existing caller passes a closure that accepts any lifetime, so they compile unchanged. The
recursion already walks children borrowed from `&'s self`.

Then add to `DecorationStore`, next to `decorations_in`:

```rust
    /// Every range touching `range`, borrowed, in ascending `(start, id)` order:
    /// the allocation-free sibling of [`Self::decorations_in`] for per-row
    /// render queries.
    pub(crate) fn visit_in<'s>(&'s self, range: Range<u32>, mut f: impl FnMut(Range<u32>, &'s DecorationKind)) {
        let (qs, qe) = (range.start, range.end);
        self.tree.filter_visit::<StartDim, _, _>(
            &|before: &StartDim, sum: &DecoSummary| before.0 <= qe && before.0 + sum.max_end >= qs,
            &mut |it: &'s DecoItem, before: &StartDim| {
                let start = before.0 + it.gap;
                let end = start + it.len;
                if start <= qe && end >= qs {
                    crate::perf::charge(1);
                    count_visit();
                    f(start..end, &it.kind);
                }
            },
        );
    }
```

It charges and counts per hit exactly like `decorations_in`, so the perf meter and the
`DECORATION_VISITS` canary see row queries (Phase 3's memo test relies on that count). Its first
caller is `inlays_in` (6h); Phase 3's row filter keeps the `&'s DecorationKind` in its spans.

5h. Tests in decorations.rs `mod tests` (add `use crate::intel::inlay;` there). Helpers, one
per anchor kind rather than one taking a `bool`:

```rust
    fn hint() -> Arc<inlay::Hint> {
        let label = vec![inlay::Part::new("h", inlay::Link::None)];
        Arc::new(inlay::Hint::new(inlay::Kind::Other, label, inlay::Key::new(0)).expect("visible"))
    }

    fn on_token(side: inlay::Side) -> DecorationKind {
        DecorationKind::InlayHint(inlay::Anchor::token(hint(), side, 0))
    }

    fn at_point(side: inlay::Side) -> DecorationKind {
        DecorationKind::InlayHint(inlay::Anchor::point(hint(), side, 0))
    }

    /// One `(range, kind, stickiness)` store item.
    fn item(range: Range<u32>, kind: DecorationKind, stickiness: Stickiness) -> (Range<u32>, DecorationKind, Stickiness) {
        (range, kind, stickiness)
    }
```

```rust
    /// A hint anchored to a token goes when the token collapses; a zero-width
    /// fallback hint keeps.
    #[test]
    fn anchored_inlay_hints_drop_on_collapse_and_point_hints_keep() {
        assert_eq!(on_token(inlay::Side::Suffix).empty_policy(), EmptyPolicy::Drop, "anchored drops");
        assert_eq!(at_point(inlay::Side::Prefix).empty_policy(), EmptyPolicy::Keep, "a point keeps");
    }

    /// One edit replacing a whole anchor drops the hint even where its
    /// stickiness would keep it; a partial replacement keeps it; the naive
    /// (multi-edit) mover agrees.
    #[test]
    fn an_edit_replacing_a_whole_anchor_drops_the_hint_on_both_movers() {
        let prefix = || {
            let mut s = store();
            s.replace_all([item(4..6, on_token(inlay::Side::Prefix), Stickiness::GrowsOnlyBefore)]);
            s
        };
        let survives = |patch: Patch| {
            let mut s = prefix();
            s.apply_patch(&patch);
            s.len() == 1
        };
        assert!(!survives(Patch::single(Edit { old: 4..6, new: 4..7 })), "exact replacement drops");
        assert!(!survives(Patch::single(Edit { old: 3..7, new: 3..5 })), "a wider replacement drops");
        assert!(survives(Patch::single(Edit { old: 5..6, new: 5..7 })), "a partial replacement keeps");
        assert!(survives(Patch::single(Edit { old: 6..6, new: 6..8 })), "an insert at the end keeps");
        let mut two = Patch::new();
        two.push(Edit { old: 0..1, new: 0..1 });
        two.push(Edit { old: 4..6, new: 4..7 });
        assert!(!survives(two), "the multi-edit path drops it too");
    }
```

```rust
    /// The windowed single-edit mover equals the naive one with anchored and
    /// zero-width inlay hints in the store, including edits that replace whole
    /// anchors.
    #[test]
    fn windowed_apply_patch_equals_naive_with_inlay_hints() {
        let mut next = xorshift(0x1A1A_7E57);
        let kind_of = |t: u64| -> (DecorationKind, Stickiness) {
            match t % 7 {
                0 => (DecorationKind::FindMatch, Stickiness::NeverGrows),
                1 => (DecorationKind::AutoClosePair, Stickiness::AlwaysGrows),
                2 => (diag(Severity::Error), Stickiness::GrowsOnlyAfter),
                3 => (on_token(inlay::Side::Suffix), Stickiness::GrowsOnlyAfter),
                4 => (on_token(inlay::Side::Prefix), Stickiness::GrowsOnlyBefore),
                5 => (at_point(inlay::Side::Suffix), Stickiness::GrowsOnlyAfter),
                _ => (at_point(inlay::Side::Prefix), Stickiness::NeverGrows),
            }
        };
        let proj = |s: &DecorationStore| -> Vec<(u64, u32, u32)> {
            s.iter().map(|r| (r.id.0, r.range.start, r.range.end)).collect()
        };
        for trial in 0..4000u32 {
            let n = next() % 40;
            let mut items = Vec::new();
            for _ in 0..n {
                let a = (next() % 120) as u32;
                let (kind, stick) = kind_of(next());
                let len = match kind.empty_policy() {
                    // The store never holds a collapsed Drop range.
                    EmptyPolicy::Drop => 1 + (next() % 24) as u32,
                    EmptyPolicy::Keep if matches!(kind, DecorationKind::InlayHint(_)) => 0,
                    EmptyPolicy::Keep => (next() % 25) as u32,
                };
                items.push(item(a..a + len, kind, stick));
            }
            let (mut windowed, mut naive) = (store(), store());
            windowed.replace_all(items.clone());
            naive.replace_all(items);
            let os = (next() % 120) as u32;
            let oe = os + (next() % 15) as u32;
            let ins = (next() % 15) as u32;
            let patch = Patch::single(Edit { old: os..oe, new: os..os + ins });
            windowed.apply_patch(&patch);
            naive.apply_patch_naive(&patch);
            assert_eq!(proj(&windowed), proj(&naive), "trial {trial}: edit {os}..{oe} → +{ins}");
        }
    }

    /// Ids follow item order, so at one start the items keep their order.
    #[test]
    fn replace_all_mints_ids_in_item_order() {
        let mut s = store();
        s.replace_all([
            item(9..9, DecorationKind::AutoClosePair, Stickiness::NeverGrows),
            item(2..2, DecorationKind::AutoClosePair, Stickiness::NeverGrows),
            item(2..2, DecorationKind::SnippetStop { index: 7 }, Stickiness::NeverGrows),
        ]);
        let order: Vec<(u32, u64)> = s.iter().map(|r| (r.range.start, r.id.0)).collect();
        assert_eq!(order, vec![(2, 2), (2, 3), (9, 1)], "sorted by start, ties in item order");
        s.clear();
        assert!(s.is_empty(), "clear empties the store");
    }

    /// Hints never enter a store through the bulk producers.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "inlay hints enter a store only through")]
    fn add_decoration_rejects_the_inlay_hint_kind() {
        store().add_decoration(0..1, on_token(inlay::Side::Suffix), Stickiness::GrowsOnlyAfter);
    }
```

`store`, `diag`, `xorshift` and `Edit` already exist in that test module (decorations.rs:1004-1006,
1582-1594). `xorshift` is defined after the oracle; Rust doesn't care about order within a module.

### Step 6 — `crates/scrive-core/src/document.rs`

6a. Imports, document.rs:12-32. Add `use crate::intel::inlay;` after
`use crate::history::{GroupingHint, History};`. `Point`, `Revision`, `DecorationKind` and
`DecorationStore` are already imported.

6b. Fields. After `autoclose: DecorationStore,` (document.rs:66):

```rust
    /// Inlay hints, in their own store so installing a set never re-sorts
    /// diagnostics or find matches. Each range covers the token its hint
    /// annotates, and rides every edit, undo and redo through the one mover.
    inlays: DecorationStore,
    /// The revision the inlay set was installed at; `None` when none is.
    inlays_at: Option<Revision>,
```

6c. `Document::new`, after `autoclose: DecorationStore::new(),` (document.rs:315):

```rust
            inlays: DecorationStore::new(),
            inlays_at: None,
```

6d. `edit_grouped`'s `Views` literal, document.rs:1124-1131. Current:

```rust
                &mut Views {
                    highlight: &mut self.highlight,
                    brackets: &mut self.brackets,
                    decorations: &mut self.decorations,
                    autoclose: &mut self.autoclose,
                    folds: &mut self.folds,
                    find: &mut self.find,
                },
```

New: add `inlays: &mut self.inlays,` after the `autoclose` line.

6e. `undo` (document.rs:1230-1234) and `redo` (:1274-1278). Current, in both:

```rust
        let Self {
            history, buffer, selections, highlight, brackets, decorations, autoclose, folds, find,
            changes, ..
        } = self;
        let mut views = Views { highlight, brackets, decorations, autoclose, folds, find };
```

New, in both:

```rust
        let Self {
            history, buffer, selections, highlight, brackets, decorations, autoclose, inlays, folds,
            find, changes, ..
        } = self;
        let mut views = Views { highlight, brackets, decorations, autoclose, inlays, folds, find };
```

6f. `Views`, document.rs:2469-2479. Add after the `autoclose` field:

```rust
    /// The inlay-hint store, a separate [`DecorationStore`] moved beside
    /// `decorations`.
    inlays: &'a mut DecorationStore,
```

6g. `rebase_views`, after `views.autoclose.apply_patch(committed.patch());` (document.rs:2562):

```rust
    views.inlays.apply_patch(committed.patch());
```

Place it before `views.find.on_commit(...)` (:2567); the find repair reads only `decorations`.

6h. The API. Put it right after `diagnostics_in` (document.rs:1970-1987), so the two
language-service publish/read pairs sit together:

```rust
    /// Install the inlay hints computed against `revision`, replacing the
    /// previous set. When the document has moved past `revision` this returns
    /// [`inlay::Outcome::Stale`] and changes nothing. Offsets are clipped to
    /// the buffer. Each hint is anchored to the token it annotates and moves
    /// with edits from then on; it goes when that token is deleted or replaced
    /// whole. The order of `hints` is the render order among hints at one
    /// offset.
    pub fn set_inlays(&mut self, revision: Revision, hints: Vec<inlay::Placed>) -> inlay::Outcome {
        let current = self.buffer.revision();
        if revision != current {
            return inlay::Outcome::Stale { current };
        }
        let count = hints.len();
        let anchored = inlay::Anchor::install(&self.buffer, hints);
        self.inlays.replace_all(
            anchored.into_iter().map(|(range, anchor, stickiness)| (range, DecorationKind::InlayHint(anchor), stickiness)),
        );
        self.inlays_at = Some(current);
        inlay::Outcome::Applied { count }
    }

    /// Remove every inlay hint.
    pub fn clear_inlays(&mut self) {
        self.inlays.clear();
        self.inlays_at = None;
    }

    /// The revision the current inlay set was installed at, or `None` when
    /// there is none. While it differs from [`revision`](Self::revision) the
    /// hints have moved with edits since their host computed them.
    #[must_use]
    pub fn inlays_revision(&self) -> Option<Revision> {
        self.inlays_at
    }

    /// Remove the hint keyed `key` that renders at `offset`, and say whether
    /// there was one. `offset` is exact only at the revision the caller read
    /// it at.
    pub fn remove_inlay(&mut self, key: inlay::Key, offset: u32) -> bool {
        if self.inlays.is_empty() {
            return false;
        }
        let row = self.buffer.offset_to_point(offset).row;
        let row_start = self.buffer.point_to_offset(Point::new(row, 0));
        let line = self.buffer.line(row);
        let row_end = row_start + line.len() as u32;
        let mut found = false;
        let taken = self.inlays.take_matching_in(offset..offset, |r| {
            let hit = !found
                && matches!(&r.kind, DecorationKind::InlayHint(anchor)
                    if anchor.hint().key() == key
                        && anchor.render_offset(r.range.clone(), row_start, row_end, &line) == Some(offset));
            found |= hit;
            hit
        });
        !taken.is_empty()
    }

    /// The inlay hints that render within `range` (touching counts), in render
    /// order: by offset, and at one offset [`Prefix`](inlay::Side::Prefix)
    /// hints before [`Suffix`](inlay::Side::Suffix) hints, each in install
    /// order. Walks the rows `range` spans.
    pub fn inlays_in(&self, range: Range<u32>) -> impl Iterator<Item = inlay::Shown> {
        let mut shown = Vec::new();
        if !self.inlays.is_empty() {
            let len = self.buffer.len();
            let first = self.buffer.offset_to_point(range.start.min(len)).row;
            let last = self.buffer.offset_to_point(range.end.min(len)).row;
            for row in first..=last {
                let row_start = self.buffer.point_to_offset(Point::new(row, 0));
                let line = self.buffer.line(row);
                let row_end = row_start + line.len() as u32;
                self.inlays.visit_in(row_start..row_end, |stored, kind| {
                    let DecorationKind::InlayHint(anchor) = kind else { return };
                    let Some(offset) = anchor.render_offset(stored, row_start, row_end, &line) else { return };
                    if (range.start..=range.end).contains(&offset) {
                        shown.push(anchor.shown(offset));
                    }
                });
            }
        }
        shown.sort_by_key(inlay::Shown::render_order);
        shown.into_iter()
    }
```

Why every hint rendered at `offset` is in `take_matching_in(offset..offset, …)`'s window: a suffix
renders at `min(end, row end)` with its start on the row, so its range contains the offset; a prefix
renders inside `[start, end]`. Both touch `offset..offset`.

The `found` flag makes `remove_inlay` take at most one hint even when two hints with one key share
an offset (keys are the host's; core doesn't enforce uniqueness).

6i. Tests in document.rs `mod tests` (it already has `use super::*;` and `doc(s)` at :2684;
`super::*` brings in the `inlay` module that 6a imports). Helpers:

```rust
    fn hint(kind: inlay::Kind, label: &str, key: u64) -> inlay::Hint {
        inlay::Hint::new(kind, vec![inlay::Part::new(label, inlay::Link::None)], inlay::Key::new(key)).expect("a visible label")
    }

    fn type_hint(label: &str, key: u64) -> inlay::Hint {
        hint(inlay::Kind::Type, label, key)
    }

    fn param_hint(label: &str, key: u64) -> inlay::Hint {
        hint(inlay::Kind::Parameter, label, key).padding(inlay::Padding { left: false, right: true })
    }

    /// Install `hints` at the current revision.
    fn install(d: &mut Document, hints: Vec<(u32, inlay::Hint)>) {
        let count = hints.len();
        let placed = hints.into_iter().map(|(offset, hint)| inlay::Placed::new(offset, hint)).collect();
        let revision = d.revision();
        assert_eq!(d.set_inlays(revision, placed), inlay::Outcome::Applied { count }, "a current set installs");
    }

    /// The text with every shown hint spliced in at its render offset,
    /// padding as spaces: what a reader sees, minus colour.
    fn with_hints(d: &Document) -> String {
        let text = d.text().into_owned();
        let (mut out, mut at) = (String::new(), 0);
        for shown in d.inlays_in(0..d.buffer().len()) {
            let offset = shown.offset() as usize;
            out.push_str(&text[at..offset]);
            at = offset;
            let padding = shown.hint().padded();
            out.push_str(if padding.left { " " } else { "" });
            shown.hint().parts().iter().for_each(|part| out.push_str(part.text()));
            out.push_str(if padding.right { " " } else { "" });
        }
        out.push_str(&text[at..]);
        out
    }

    fn caret(d: &mut Document, offset: u32) {
        d.set_selections(SelectionSet::new(offset));
    }

    fn select(d: &mut Document, range: Range<u32>) {
        d.set_selections(SelectionSet::from_ranges(&[(range.start, range.end)], 0));
    }
```

The tests (each with a `///` invariant line and string assert messages):

```rust
    /// Typing at a suffix hint's offset extends the token it follows, so the
    /// text lands before the hint.
    #[test]
    fn typing_at_a_suffix_hint_lands_before_it() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(5, type_hint(": i32", 1))]);
        assert_eq!(with_hints(&d), "let x: i32 = 1;");
        caret(&mut d, 5);
        d.type_char('y');
        assert_eq!(with_hints(&d), "let xy: i32 = 1;", "typed text lands before a suffix hint");
    }

    /// Typing at a prefix hint's offset extends the token it precedes, so the
    /// text lands after the hint.
    #[test]
    fn typing_at_a_prefix_hint_lands_after_it() {
        let mut d = doc("foo(x)");
        install(&mut d, vec![(4, param_hint("n:", 1))]);
        assert_eq!(with_hints(&d), "foo(n: x)");
        caret(&mut d, 4);
        d.type_char('y');
        assert_eq!(with_hints(&d), "foo(n: yx)", "typed text lands after a prefix hint");
    }

    /// Enter at the end of a hinted line grows the suffix range over the
    /// newline and the indent, but the hint stays at the end of its line.
    #[test]
    fn enter_at_the_end_of_a_line_keeps_its_hint_on_that_line() {
        let mut d = doc("    foo()");
        install(&mut d, vec![(9, type_hint(": u8", 1))]);
        caret(&mut d, 9);
        d.enter();
        assert_eq!(d.text(), "    foo()\n    ");
        assert_eq!(with_hints(&d), "    foo(): u8\n    ", "the hint stays on its line, not right of the caret");
    }

    /// Enter typed before an argument carries its prefix hint to the
    /// argument's new line, past the indent.
    #[test]
    fn enter_before_an_argument_moves_its_prefix_hint_to_the_first_non_blank() {
        let mut d = doc("    foo(a)");
        install(&mut d, vec![(8, param_hint("n:", 1))]);
        caret(&mut d, 8);
        d.enter();
        let a = d.text().find('a').expect("the argument survives") as u32;
        let shown: Vec<u32> = d.inlays_in(0..d.buffer().len()).map(|s| s.offset()).collect();
        assert_eq!(shown, vec![a], "the hint renders before the argument, not before the indent");
    }

    /// Backspacing the last char of an annotated word shrinks the anchor; the
    /// hint stays (a typo fix doesn't make the line jump).
    #[test]
    fn backspace_inside_the_anchor_word_keeps_the_hint() {
        let mut d = doc("let count = 1;");
        install(&mut d, vec![(9, type_hint(": i32", 1))]);
        caret(&mut d, 9);
        d.backspace();
        assert_eq!(with_hints(&d), "let coun: i32 = 1;", "the hint follows the shortened word");
    }

    /// Deleting the whole annotated token drops its hint, and undo brings the
    /// text back without it.
    #[test]
    fn deleting_the_anchor_token_drops_the_hint_and_undo_does_not_restore_it() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(5, type_hint(": i32", 1))]);
        caret(&mut d, 5);
        d.backspace();
        assert_eq!(with_hints(&d), "let  = 1;", "the hint went with its token");
        assert!(d.undo());
        assert_eq!(with_hints(&d), "let x = 1;", "undo restores the text, not the hint");
        assert!(d.redo());
        assert_eq!(d.inlays_in(0..d.buffer().len()).count(), 0, "redo brings nothing back either");
        assert_ne!(d.inlays_revision(), Some(d.revision()), "the set is no longer current");
    }

    /// A hint that survives an edit rides that edit's undo and redo.
    #[test]
    fn a_surviving_hint_rides_undo_and_redo() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(5, type_hint(": i32", 1))]);
        caret(&mut d, 5);
        d.type_char('y');
        assert!(d.undo());
        assert_eq!(with_hints(&d), "let x: i32 = 1;", "undo moves the hint back");
        assert!(d.redo());
        assert_eq!(with_hints(&d), "let xy: i32 = 1;", "redo moves it forward again");
    }

    /// A suffix hint at a line start and a prefix hint at a line end have no
    /// token beside them; they keep a zero-width anchor with the same typing
    /// rule.
    #[test]
    fn hints_without_a_neighbour_on_their_line_keep_a_zero_width_anchor() {
        let mut d = doc("ab\ncd");
        install(&mut d, vec![(3, type_hint("S", 1)), (5, param_hint("P", 2))]);
        assert_eq!(with_hints(&d), "ab\nScdP ");
        caret(&mut d, 3);
        d.type_char('x');
        assert_eq!(with_hints(&d), "ab\nxScdP ", "text typed at a suffix point lands before it");
        caret(&mut d, 6);
        d.type_char('y');
        assert_eq!(with_hints(&d), "ab\nxScdP y", "text typed at a prefix point lands after it");
    }

    /// A paste over a selection replaces whole tokens; the hints anchored to
    /// them go, on either side.
    #[test]
    fn pasting_over_a_selection_drops_the_hints_anchored_there() {
        let mut d = doc("foo(count, 1)");
        install(&mut d, vec![(4, param_hint("n:", 1)), (9, type_hint(": i32", 2)), (11, param_hint("m:", 3))]);
        select(&mut d, 4..9);
        d.paste("total", false);
        assert_eq!(with_hints(&d), "foo(total, m: 1)", "both hints on the replaced word go");
    }

    /// A host edit that replaces a whole line drops the hints on it and leaves
    /// the other lines' hints alone.
    #[test]
    fn a_line_replacing_edit_drops_the_hints_on_that_line() {
        let mut d = doc("foo(x)\nbar(y)\n");
        install(&mut d, vec![(4, param_hint("a:", 1)), (11, param_hint("b:", 2))]);
        d.edit(vec![EditOp::new(0..6, "foo(z)")]).unwrap();
        assert_eq!(with_hints(&d), "foo(z)\nbar(b: y)\n", "only the replaced line loses its hint");
    }

    /// Retyping a selected word drops its hint whichever side the hint is on;
    /// stickiness alone would keep the prefix one at an arbitrary byte.
    #[test]
    fn retyping_a_selected_word_drops_its_hint_on_either_side() {
        let mut d = doc("let x = f(y);");
        install(&mut d, vec![(5, type_hint(": i32", 1)), (10, param_hint("n:", 2))]);
        select(&mut d, 4..5);
        d.type_char('z');
        select(&mut d, 10..11);
        d.type_char('w');
        assert_eq!(with_hints(&d), "let z = f(w);", "both retyped tokens lost their hints");
    }

    /// Alt+↓ replaces both swapped lines in one edit, so their hints drop
    /// until the next fetch; a third line keeps its hint.
    #[test]
    fn moving_a_line_drops_the_hints_on_both_swapped_lines() {
        let mut d = doc("a(x)\nb(y)\nc(z)\n");
        install(&mut d, vec![(2, param_hint("p:", 1)), (7, param_hint("q:", 2)), (12, param_hint("r:", 3))]);
        caret(&mut d, 0);
        d.move_line(true);
        assert_eq!(with_hints(&d), "b(y)\na(x)\nc(r: z)\n", "the swapped lines lost their hints");
    }

    /// `Other` hints left at `Auto` follow the text: a prefix where a word
    /// starts and none ends, a suffix elsewhere (Zed's
    /// `test_none_kind_hint_bias` cases, plus an expression-start adjustment).
    /// The padding only widens them; the client's `.placement` is not set here.
    #[test]
    fn auto_placement_follows_the_text_around_the_hint() {
        let other = |label: &str, left, right| {
            hint(inlay::Kind::Other, label, 1).padding(inlay::Padding { left, right })
        };
        let cases = [
            ("fn foo(s: &str) {}", 6, other("<'_>", false, false), "fn fooX<'_>(s: &str) {}"),
            ("fn foo(s: &str) {}", 11, other("'_", false, true), "fn foo(s: &'_ Xstr) {}"),
            ("fn foo(s: &str) {}", 18, other("// fn foo", true, false), "fn foo(s: &str) {}X // fn foo"),
            ("let g = ||f;", 10, other("<fn-item-to-fn-pointer>", false, false), "let g = ||<fn-item-to-fn-pointer>Xf;"),
        ];
        for (text, at, h, typed) in cases {
            let mut d = doc(text);
            install(&mut d, vec![(at, h)]);
            caret(&mut d, at);
            d.type_char('X');
            assert_eq!(with_hints(&d), typed, "{text} at {at}");
        }
    }

    /// Hints at one fetch offset that want both sides all become suffixes in
    /// server order (Zed's `test_colocated_mixed_kind_hints_share_bias`).
    #[test]
    fn mixed_sides_at_one_offset_render_as_suffixes_in_server_order() {
        let text = "fn f() {} fn main() { let c: fn() -> fn() = ||f; }";
        let at = text.find("||f").expect("the closure") as u32 + 2;
        let mut d = doc(text);
        install(&mut d, vec![(at, type_hint(" -> fn()", 1)), (at, hint(inlay::Kind::Other, "<fn-item-to-fn-pointer>", 2))]);
        assert_eq!(with_hints(&d), "fn f() {} fn main() { let c: fn() -> fn() = || -> fn()<fn-item-to-fn-pointer>f; }");
        assert!(d.inlays_in(0..d.buffer().len()).all(|s| s.side() == inlay::Side::Suffix), "both are suffixes");
        caret(&mut d, at);
        d.type_char('X');
        assert_eq!(
            with_hints(&d),
            "fn f() {} fn main() { let c: fn() -> fn() = ||X -> fn()<fn-item-to-fn-pointer>f; }",
            "typed text lands before the whole group",
        );
    }

    /// Unsorted answers render in offset order; hints at one offset render in
    /// the order the server sent them, whatever their keys.
    #[test]
    fn hints_render_in_offset_order_and_in_server_order_at_one_offset() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(9, type_hint("C", 7)), (5, type_hint("B", 9)), (5, type_hint("A", 3))]);
        let keys: Vec<inlay::Key> = d.inlays_in(0..d.buffer().len()).map(|s| s.key()).collect();
        assert_eq!(keys, vec![inlay::Key::new(9), inlay::Key::new(3), inlay::Key::new(7)], "offset, then server order");
        assert_eq!(with_hints(&d), "let xBA = 1C;");
    }

    /// A set computed for an older revision is refused and leaves the current
    /// set in place.
    #[test]
    fn a_stale_hint_set_is_refused_and_changes_nothing() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(5, type_hint(": i32", 1))]);
        let installed = d.revision();
        d.edit(vec![EditOp::insert(10, "\n")]).unwrap();
        let stale = d.set_inlays(installed, vec![inlay::Placed::new(5, type_hint(": u8", 2))]);
        assert_eq!(stale, inlay::Outcome::Stale { current: d.revision() }, "an old revision is refused");
        assert_eq!(with_hints(&d), "let x: i32 = 1;\n", "the installed set is untouched");
        assert_eq!(d.inlays_revision(), Some(installed), "the set keeps its install revision");
    }

    /// Clearing empties the set and forgets the revision it was installed at.
    #[test]
    fn clear_inlays_empties_the_set_and_forgets_its_revision() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(5, type_hint(": i32", 1))]);
        assert_eq!(d.inlays_revision(), Some(d.revision()));
        d.clear_inlays();
        assert_eq!(d.inlays_in(0..d.buffer().len()).count(), 0, "no hints left");
        assert_eq!(d.inlays_revision(), None, "no install revision either");
    }

    /// `remove_inlay` takes the one keyed hint at its render offset and
    /// nothing else.
    #[test]
    fn remove_inlay_takes_only_the_keyed_hint_at_its_render_offset() {
        let mut d = doc("let x = 1;");
        install(&mut d, vec![(5, type_hint("A", 1)), (5, type_hint("B", 2))]);
        assert!(!d.remove_inlay(inlay::Key::new(1), 4), "a wrong offset removes nothing");
        assert!(!d.remove_inlay(inlay::Key::new(3), 5), "an unknown key removes nothing");
        assert!(d.remove_inlay(inlay::Key::new(1), 5), "the keyed hint goes");
        assert_eq!(with_hints(&d), "let xB = 1;", "its neighbour stays");
        assert!(!d.remove_inlay(inlay::Key::new(1), 5), "and it is gone for good");
    }

    /// Offsets past the end or inside a char are clipped before anchoring.
    #[test]
    fn installed_hint_offsets_are_clipped_to_the_buffer() {
        let mut d = doc("aé");
        install(&mut d, vec![(99, type_hint("E", 1)), (2, type_hint("M", 2))]);
        let offsets: Vec<u32> = d.inlays_in(0..u32::MAX).map(|s| s.offset()).collect();
        assert_eq!(offsets, vec![1, 3], "mid-char snaps left, past-the-end clamps");
    }
```

Fixture checks for the tests above (byte offsets):

- `"let x = 1;"`: `x` is 4, so 5 is just after it.
- `"foo(count, 1)"`: `count` is 4..9, `1` is 11. The paste replaces 4..9 with `total`: the prefix
  `n:` (anchored 4..9, L/L) and the suffix `: i32` (anchored 4..9, R/R) are both covered.
- `"foo(x)\nbar(y)\n"`: `y` is 11.
- `"let x = f(y);"`: `x` is 4, `y` is 10. After the first retype the text length is unchanged,
  so `y` stays at 10.
- `"a(x)\nb(y)\nc(z)\n"`: `x` 2, `y` 7, `z` 12.
- `"let g = ||f;"`: `f` is 10; its left neighbour `|` is not a word char, so `Auto` → `Prefix`.
- `"ab\ncd"`: 3 is the start of row 1 (suffix fallback), 5 is EOF (prefix fallback). After the
  first edit the text is `"ab\nxcd"`, so EOF is 6.
- `"aé"`: `é` is bytes 1..3, so 2 snaps left to 1; 99 clamps to 3.

If an assertion's expected string disagrees with what the code does, check the fixture offsets
first, then the rules in §3. Don't change a rule to make a test pass; report it.

### Step 7 — `crates/scrive-core/src/perf_gate.rs`

Append after the last cell (`diagnostic_overview_is_diag_count_independent`). Import what you need
at the top: `use crate::intel::inlay;`.

```rust
// ── INLAY HINTS. The hints live in their own store and ride the windowed
//    mover, so typing next to one costs the same whatever the set's size, and
//    never re-sorts a store. Installing a set is one sort, linear in its size. ──

/// A `k`-block document with a type hint after each block's `fn`.
fn hinted(k: usize) -> Document {
    let mut d = build(k);
    let placed: Vec<inlay::Placed> = (0..k).map(|i| inlay::Placed::new(block_start(i) + 2, type_hint(i))).collect();
    let revision = d.revision();
    let _ = d.set_inlays(revision, placed);
    d
}

fn type_hint(i: usize) -> inlay::Hint {
    let label = vec![inlay::Part::new(": u8", inlay::Link::None)];
    inlay::Hint::new(inlay::Kind::Type, label, inlay::Key::new(i as u64)).expect("a visible label")
}

#[test]
fn typing_next_to_a_hint_is_hint_count_independent() {
    use crate::decorations::DECORATION_SORTS;
    let (s, b) = (1000usize, 2000usize);
    let cell = |k: usize| {
        let mut d = hinted(k);
        d.set_selections(SelectionSet::new(block_start(k / 2) + 2));
        let sorts = DECORATION_SORTS.with(std::cell::Cell::get);
        let work = meter_of(&mut d, |d| d.type_char('x'));
        assert_eq!(DECORATION_SORTS.with(std::cell::Cell::get), sorts, "a keystroke never re-sorts a store");
        work
    };
    assert_budget("inlays: type at a hint, k hints", Budget::Constant, cell(s), cell(b));
}

#[test]
fn installing_hints_is_linear() {
    const BLOCKS: usize = 2000;
    let (s, b) = (1000usize, 2000usize);
    let cell = |k: usize| {
        let mut d = build(BLOCKS);
        let placed: Vec<inlay::Placed> = (0..k).map(|i| inlay::Placed::new(block_start(i) + 2, type_hint(i))).collect();
        let revision = d.revision();
        meter_of(&mut d, |d| {
            let _ = d.set_inlays(revision, placed);
        })
    };
    assert_budget("inlays: install, k hints", Budget::Linear, cell(s), cell(b));
}
```

The typing cell also doubles the document with `k` (`build(k)`); the existing
`*_size_independent` cells already show a keystroke is flat in document size, so the only new
signal is the hint count. Typing `x` after `fn` makes `fnx`, extending the suffix anchor.

## 5. Files changed

| File | Change | Commit |
|---|---|---|
| crates/scrive-core/src/intel.rs | `pub mod inlay;`, module doc names inlay hints | 1 |
| crates/scrive-core/src/intel/inlay.rs (new) | `Kind`, `Placement`, `Side`, `Padding`, `Link`, `Insert`, `Key`, `Part`, `Hint`, `Placed`, `Outcome`, `Error`; tests | 1 |
| crates/scrive-core/src/intel/inlay/request.rs (new) | `Request` | 1 |
| crates/scrive-core/src/intel/inlay/interaction.rs (new) | `Interaction`, `Gesture` | 1 |
| crates/scrive-core/src/lib.rs | "Where do I…" bullet | 1 |
| crates/scrive-core/src/intel/inlay.rs | `Hint`'s `placement` field and `.placement()` setter, `default_placement`; `Anchor`, `Shown`, `Placed::into_parts`, install helpers; render-offset and placement tests | 2 |
| crates/scrive-core/src/decorations.rs | `InlayHint` variant, `empty_policy` arm, whole-anchor drop in `remap_ranges`, bulk-insert `debug_assert!`s, `replace_all`, `clear`, `visit_in`; tests | 2 |
| crates/scrive-core/src/sum_tree.rs | `filter_visit` / `filter_visit_from` tie the visited item's borrow to `&self` (R8) | 2 |
| crates/scrive-core/src/document.rs | `inlays` store + `inlays_at`; `Views`/`rebase_views`/undo/redo wiring; `set_inlays`, `clear_inlays`, `inlays_revision`, `remove_inlay`, `inlays_in`; tests | 2 |
| crates/scrive-core/src/perf_gate.rs | two inlay cells | 3 |

## 6. Verification

At each boundary:

```
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p scrive-core
```

At the end of the phase:

```
cargo test --workspace
cargo test --workspace --all-features
cargo test -p scrive-core -- inlay hint perf_gate
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo build --workspace --all-targets --all-features --target wasm32-unknown-unknown
out=$(cargo tree -p scrive-core -e normal --prefix none) || exit 1; if grep -qE '^(iced|winit|wgpu|lsp-types)' <<<"$out"; then echo LEAK; exit 1; fi
rustfmt --edition 2021 --check crates/scrive-core/src/intel/inlay.rs crates/scrive-core/src/intel/inlay/request.rs crates/scrive-core/src/intel/inlay/interaction.rs
```

(`rustfmt` without `--check` on those three new files is allowed; never on other files.) The test
count rises from 901 by the tests this phase adds; nothing else in the suite changes.

## 7. Spot-check tables

### Anchoring at install (`Anchor::install`)

| Text | `p` | Kind / padding | Side | Stored range | Stickiness | Anchored |
|---|---|---|---|---|---|---|
| `let count = 1;` | 9 | Type | Suffix | 4..9 | GrowsOnlyAfter | yes (word) |
| `foo(x)` | 4 | Parameter, `false/true` | Prefix | 4..5 | GrowsOnlyBefore | yes (word) |
| `ab()` | 4 | Type | Suffix | 3..4 (`)`) | GrowsOnlyAfter | yes (one char) |
| `ab\ncd` | 3 | Type | Suffix | 3..3 | GrowsOnlyAfter | no (line start) |
| `ab\ncd` | 2 | Parameter | Prefix | 2..2 | NeverGrows | no (line end) |
| `fn foo(s: &str) {}` | 6 | Other (default `Auto`) | Auto → Suffix | 3..6 | GrowsOnlyAfter | yes |
| `fn foo(s: &str) {}` | 11 | Other, `.placement(Prefix)` | Prefix | 11..14 | GrowsOnlyBefore | yes |
| `let g = ||f;` | 10 | Other (default `Auto`) | Auto → Prefix | 10..11 | GrowsOnlyBefore | yes |
| `…= ||f; }` | at `f` | Type + Other at one offset | mixed → both Suffix | `|`..`f` (one char) | GrowsOnlyAfter | yes |

### The mover on an anchored hint

| Before | Edit | Suffix `[ws, p)` (R,R) | Prefix `[p, we)` (L,L) |
|---|---|---|---|
| `let x` / `x` | insert `y` at `p` (Suffix p=5; Prefix p=4) | `4..6`, renders at 6 | `4..6`, renders at 4 |
| `let count` | delete `t` (8..9) | `4..8` kept | — |
| `let x` | delete `x` (4..5) | collapses → dropped | collapses → dropped |
| `x` at 4..5 | replace 4..5 with `z` | `5..5` → dropped (collapse) | `4..5` → dropped (whole-anchor rule) |
| `count` at 4..9 | replace 3..10 with 2 bytes | dropped (covered) | dropped (covered) |
| `count` at 4..9 | replace 5..9 with 3 bytes | kept `4..8` | kept `4..8` |
| `foo()` + EOL hint | Enter at EOL (`\n    `) | `8..14`, renders at 9 (row clamp) | — |
| `    foo(a)` | Enter at 8 (`\n    `) | — | `8..14`, renders at 13 (first non-blank) |

### `render_offset`

| Side | Range | Row start / line | Result |
|---|---|---|---|
| Suffix | 8..14 | 0 / `    foo()` | `Some(9)` |
| Suffix | 8..14 | 10 / `    ` | `None` |
| Prefix | 8..14 | 9 / `    a)` | `Some(13)` |
| Prefix | 8..14 | 0 / `    foo(` | `None` |
| Prefix | 8..9 | 0 / `    foo(a)` | `Some(8)` |
| Prefix (point) | 4..4 | 0 / `foo(` | `Some(4)` |

### `set_inlays` outcomes

| Call | Outcome | Store | `inlays_revision()` |
|---|---|---|---|
| `set_inlays(revision(), hints)` | `Applied { count }` | replaced | `Some(revision())` |
| `set_inlays(older, hints)` | `Stale { current }` | unchanged | unchanged |
| `set_inlays(revision(), vec![])` | `Applied { count: 0 }` | empty | `Some(revision())` |
| `clear_inlays()` | — | empty | `None` |
| any edit after install | — | moved | unchanged (now ≠ `revision()`) |

## 8. What NOT to change

- No `RowLayout`, `FoldMap`, `HeaderLayout`, `row_layout::Rows` or `Edge` work: Phase 2 adds the
  view and edges, Phase 3 lays hints out. Hints never enter the `FoldMap` or its cache key.
- No mid-word render guard anywhere (R11).
- No scrive-iced or scrive-lsp change. No new dependency; no `lsp-types` in scrive-core.
- Don't touch the existing oracle `windowed_apply_patch_equals_naive_under_random_single_edits` or
  `only_find_match_drops_on_collapse`; the hint cases are new tests.
- Don't change `DiagnosticsOutcome`, `set_diagnostics`, `decorations()` or `decorations_mut()`.
- Don't expose the inlay store, `Anchor`'s methods or any `TrackedRange` of the inlay store
  publicly. Don't add the inlay types to the crate-root `pub use` list.
- Don't add `#[non_exhaustive]` anywhere new, and don't remove it from `DecorationKind`.
- Don't clear hints in `reset_transient`, `undo` or `redo`: a dropped hint stays dropped and a
  surviving one rides the step. `CodeEditor::load` clearing is Phase 5.
- Never run `cargo fmt`; don't reformat lines you didn't change.

## 9. Pitfalls

- **Dead code at boundary 1.** `Anchor`, `Shown` and `Placed::into_parts` have crate-private
  methods; they land with their callers in boundary 2. `Hint`'s `placement` field has no getter, so
  it lands in boundary 2 too, beside `Anchor::install`, which reads it. Every other boundary-1 item
  is `pub` or read by a `pub` getter, so it can't warn as dead.
- **The debug_assert blocks `add_decoration` in tests.** Build stores holding hints with
  `replace_all`, never `add_decoration`/`add_sorted_batch`/`splice_sorted_batch`. The
  `should_panic` test is `#[cfg(debug_assertions)]`, so `cargo test --release` doesn't fail on it.
- **Old coordinates.** The whole-anchor test in `remap_ranges` must run in the first loop, before
  the `r.range = …` loop. Reading after the overwrite tests the new text and drops the wrong hints.
- **Equivalence.** Never store a collapsed anchored hint: `place` anchors only non-empty ranges,
  and the fallbacks are `Keep`. The new oracle test pins this; the existing one must stay green.
- **Borrows in `undo`/`redo`.** `inlays` goes into the `let Self { … } = self;` pattern and into
  `Views { … }` from the destructured binding, like `autoclose`. Don't touch `self.inlays` inside the
  history callback.
- **Borrows in `remove_inlay`.** `line` borrows `self.buffer` while `take_matching_in` borrows
  `self.inlays` mutably. That compiles because both go through `self.<field>` directly; don't route
  either through a `&self` method in between.
- **`let … else` on `DecorationKind`.** The enum is `#[non_exhaustive]`, but inside the crate a
  refutable `let DecorationKind::InlayHint(anchor) = &r.kind else { continue };` is fine.
- **Name clashes.** `inlay::Hint::padding` is the builder setter; the getter is `padded`. Likewise
  `insert` / `insertable` (a `bool`), and `placement` is a setter with no getter (the field and the
  method share the name, which Rust allows). The label getter is `parts`. `inlay::Link::None` is a
  variant, so never glob-import `Link::*`.
- **`visit_in`'s lifetime.** The `'s` on `filter_visit` is what lets Phase 3 keep
  `&'s DecorationKind` in a `RowLayout`. Don't narrow it back to a higher-ranked closure because
  `inlays_in` alone doesn't need it.
- **Doc links.** `#![deny(missing_docs)]` covers every pub item, variant and field in the three new
  files. Link `[`Document::set_inlays`](crate::Document::set_inlays)` from inlay.rs; `Document` is
  re-exported at the crate root. `RUSTDOCFLAGS="-D warnings"` fails on a broken link.
- **clippy.** Use `(a..=b).contains(&x)` (`manual_range_contains`); `then(|| …)` in
  `render_offset` (lazy); `is_some_and`; no `unwrap()` in library code (`expect` with a reason).
  The perf-gate and test helpers may use `expect`/`unwrap`.
- **Perf.** `Anchor::install` charges nothing itself; `replace_all` → `set_sorted` charges `len`,
  which is the Linear cell's signal. Don't add `offset_to_point` calls to install.
- **Revision type.** `set_inlays` and `Outcome::Stale` use `Revision`, not `u64`.

## 10. Resolved questions

1. **The render guard vs the `||` test:** the guard is removed everywhere (R11); the test keeps
   both assertions.
2. **Placement vs padding collapse:** `Hint::new` defaults placement from the kind, `.placement()`
   overrides it, and the client computes an `Other` hint's placement from the raw padding before
   collapsing it (R5). The getters are R1's.
3. **The shape of `Interaction`:** adopted, with `Gesture::Tooltip { part: u32 }` (R6).
4. **The borrowing row query** lives here, with the `filter_visit` lifetime (R8).
5. **`render_offset`'s signature** takes the range and both row bounds (R4).

Still open: none.

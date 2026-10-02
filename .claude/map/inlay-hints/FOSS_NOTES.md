# How Zed, Helix and Lapce/Floem implement LSP inlay hints: notes for scrive

## Where the code is, and caveats

`$FOSS` = `/tmp/claude-1000/-home-nickel-Programming-github-scrive/27102364-edb3-4974-b203-1d6e4802bc8d/scratchpad/foss`. Every citation below is `path:line` under `$FOSS`.

Checkout revisions:

| Repo | Commit | Date |
|---|---|---|
| zed | `dd510f99` | 2026-09-28 |
| helix | `079a789e` | 2026-07-23 |
| lapce | `b604d57d` | 2026-09-06 |
| floem | `1351ffb1` | 2026-09-23 |

- **Floem version mismatch.** Lapce pins floem at `31fa8f44…` (`lapce/Cargo.toml:79-88`). The floem checkout is HEAD at depth 1, so I could not inspect the pinned revision. The APIs have drifted:
  - Floem HEAD's `offset_of_point` returns `(usize, bool, CursorAffinity)` (`floem/src/views/editor/mod.rs:1038`).
  - Lapce destructures two values from it (`lapce/lapce-app/src/editor.rs:2821-2823`, `:2884`).
  - Everything I say about Floem describes HEAD.
- **Upstream code.** xi-rope `Spans` (which Lapce uses) is not in the checkouts. Its behaviour below comes from the upstream source at `github.com/lapce/xi-editor/rust/rope/src/{spans,tree}.rs`, which I fetched and read. It has no local line numbers.
- **Inferred bugs.** Anything marked **(inferred)** comes from reading the code. I did not reproduce it.

Main files (absolute paths):

- `$FOSS/zed/crates/editor/src/display_map/inlay_map.rs`
- `$FOSS/zed/crates/editor/src/display_map.rs`
- `$FOSS/zed/crates/editor/src/inlays.rs`
- `$FOSS/zed/crates/editor/src/inlays/inlay_hints.rs`
- `$FOSS/zed/crates/project/src/lsp_store/inlay_hints.rs`
- `$FOSS/zed/crates/project/src/lsp_store.rs`
- `$FOSS/zed/crates/project/src/lsp_command.rs`
- `$FOSS/zed/crates/language/src/buffer/row_chunk.rs`
- `$FOSS/zed/crates/editor/src/hover_popover.rs`
- `$FOSS/zed/crates/editor/src/hover_links.rs`
- `$FOSS/zed/crates/editor/src/element.rs`
- `$FOSS/zed/crates/editor/src/element/mouse.rs`
- `$FOSS/zed/crates/editor/src/movement.rs`
- `$FOSS/helix/helix-core/src/text_annotations.rs`
- `$FOSS/helix/helix-core/src/doc_formatter.rs`
- `$FOSS/helix/helix-core/src/position.rs`
- `$FOSS/helix/helix-core/src/transaction.rs`
- `$FOSS/helix/helix-view/src/document.rs`
- `$FOSS/helix/helix-view/src/view.rs`
- `$FOSS/helix/helix-term/src/commands/lsp.rs`
- `$FOSS/helix/helix-term/src/ui/document.rs`
- `$FOSS/floem/src/views/editor/phantom_text.rs`
- `$FOSS/floem/src/views/editor/mod.rs`
- `$FOSS/floem/src/views/editor/visual_line.rs`
- `$FOSS/floem/src/views/editor/layout.rs`
- `$FOSS/floem/src/views/editor/view.rs`
- `$FOSS/floem/src/views/editor/movement.rs`
- `$FOSS/floem/editor-core/src/cursor.rs`
- `$FOSS/lapce/lapce-app/src/doc.rs`
- `$FOSS/lapce/lapce-app/src/editor.rs`
- `$FOSS/lapce/lapce-proxy/src/dispatch.rs`

---

## 1. Zed

### Key types (verbatim)

```rust
// zed/crates/editor/src/display_map/inlay_map.rs:34-44, 54-58, 78-84
pub struct InlayMap {
    snapshot: InlaySnapshot,
    inlays: Vec<Inlay>,
}

#[derive(Clone)]
pub struct InlaySnapshot {
    pub buffer: MultiBufferSnapshot,
    transforms: SumTree<Transform>,
    pub version: usize,
}

#[derive(Clone, Debug)]
enum Transform {
    Isomorphic(MBTextSummary),
    Inlay(Inlay),
}

#[derive(Clone, Debug, Default)]
struct TransformSummary {
    /// Summary of the text before inlays have been applied.
    input: MBTextSummary,
    /// Summary of the text after inlays have been applied.
    output: MBTextSummary,
}

// :103, :106, :170
pub type InlayEdit = Edit<InlayOffset>;
pub struct InlayOffset(pub MultiBufferOffset);
pub struct InlayPoint(pub Point);

// :226-245
pub struct InlayChunks<'a> {
    transforms: Cursor<'a, 'static, Transform, Dimensions<InlayOffset, MultiBufferOffset>>,
    buffer_chunks: CustomHighlightsChunks<'a>,
    buffer_chunk: Option<Chunk<'a>>,
    inlay_chunks: Option<text::ChunkWithBitmaps<'a>>,
    /// text, char bitmap, tabs bitmap
    inlay_chunk: Option<ChunkBitmaps<'a>>,
    output_offset: InlayOffset,
    max_output_offset: InlayOffset,
    highlight_styles: HighlightStyles,
    highlights: Highlights<'a>,
    snapshot: &'a InlaySnapshot,
}
pub struct InlayChunk<'a> {
    pub chunk: Chunk<'a>,
    /// Whether the inlay should be customly rendered.
    pub renderer: Option<ChunkRenderer>,
}
```

Signatures (whitespace collapsed):

```rust
pub fn sync(&mut self, buffer_snapshot: MultiBufferSnapshot, mut buffer_edits: Vec<text::Edit<MultiBufferOffset>>) -> (InlaySnapshot, Vec<InlayEdit>)  // :574-578
pub fn splice(&mut self, to_remove: &[InlayId], to_insert: Vec<Inlay>) -> (InlaySnapshot, Vec<InlayEdit>)                                        // :730-734
pub fn to_buffer_point(&self, point: InlayPoint) -> Point            // :911
pub fn to_inlay_offset(&self, offset: MultiBufferOffset) -> InlayOffset // :940
pub fn to_inlay_point(&self, point: Point) -> InlayPoint             // :977
pub fn clip_point(&self, mut point: InlayPoint, mut bias: Bias) -> InlayPoint // :1045
pub fn inlay_bias_at_point(&self, point: InlayPoint) -> Option<Bias> // :1136
```

```rust
// zed/crates/editor/src/inlays.rs:34-37, 45-57
pub struct InlaySplice { pub to_remove: Vec<InlayId>, pub to_insert: Vec<Inlay> }
pub struct Inlay {
    pub id: InlayId,
    // TODO this could be an ExcerptAnchor
    pub position: Anchor,
    pub content: InlayContent,
}
pub enum InlayContent { Text(text::Rope), Color(Hsla) }

// zed/crates/project/src/project.rs:527-535, 846-850, 867-872
pub struct InlayHint {
    pub position: language::Anchor,
    pub label: InlayHintLabel,
    pub kind: Option<InlayHintKind>,
    pub padding_left: bool,
    pub padding_right: bool,
    pub tooltip: Option<InlayHintTooltip>,
    pub resolve_state: ResolveState,
}
pub enum ResolveState {
    Resolved,
    CanResolve(LanguageServerId, Option<lsp::LSPAny>),
    Resolving,
}
pub struct InlayHintLabelPart {
    pub value: String,
    pub tooltip: Option<InlayHintLabelPartTooltip>,
    pub location: Option<(LanguageServerId, lsp::Location)>,
    pub command: Option<(LanguageServerId, lsp::Command)>,
}

// zed/crates/editor/src/inlays/inlay_hints.rs:286-296
pub enum InlayHintRefreshReason {
    ModifiersChanged(bool),
    Toggle(bool),
    SettingsChange(InlayHintSettings),
    NewLinesShown,
    BufferEdited(BufferId),
    ServerRemoved,
    LanguageServerRegistered,
    RefreshRequested { server_id: LanguageServerId },
    BuffersRemoved(Vec<BufferId>),
}

// zed/crates/project/src/lsp_store/inlay_hints.rs:53-69 (doc comments trimmed), 80-89
pub enum InvalidationStrategy { RefreshRequested { server_id: LanguageServerId }, BufferEdited, None }
pub struct BufferInlayHints {
    chunks: RowChunks,
    hints_by_chunks: Vec<Option<CacheInlayHints>>,
    fetches_by_chunks: Vec<Option<CacheInlayHintsTask>>,
    hints_by_id: HashMap<InlayId, HintForId>,
    pending_refreshes: HashSet<LanguageServerId>,
    work_end_refreshes: HashSet<LanguageServerId>,
    pub(super) fetched_servers: HashSet<LanguageServerId>,
    pub(super) hint_resolves: HashMap<InlayId, Shared<Task<()>>>,
}

// zed/crates/editor/src/hover_links.rs:135-139 ; hover_popover.rs:133-136 ; element.rs:10688-10694
pub struct InlayHighlight { pub inlay: InlayId, pub inlay_position: Anchor, pub range: Range<usize> }
pub struct InlayHover { pub(crate) range: InlayHighlight, pub tooltip: HoverBlock }
pub struct PointForPosition {
    pub previous_valid: DisplayPoint,
    pub next_valid: DisplayPoint,
    pub nearest_valid: DisplayPoint,
    pub exact_unclipped: DisplayPoint,
    pub column_overshoot_after_line_end: u32,
}
```

### 1.1 Data structure and complexity

- **Structure.** There are two pieces:
  - A sorted `Vec<Inlay>`. Positions are anchors, and this is the source of truth (`inlay_map.rs:34-37`, `:754-763`).
  - A `SumTree<Transform>` that alternates `Isomorphic` buffer runs with `Inlay` transforms. An `Inlay` transform has empty `input` and the hint text as `output` (`:60-76`).
- **Invariants.** `check_invariants` asserts that `input` equals the buffer summary and that no two isomorphic transforms are adjacent (`:1297-1315`).
- **Dimensions.** `InlayOffset` and `InlayPoint` sum `output`; `MultiBufferOffset` and `Point` sum `input` (`:159-216`). One cursor therefore converts in either direction in O(log T).
- **Per edit (`sync`, `:573-727`).** For each buffer edit it:
  - slices the tree to the edit and re-summarises the prefix;
  - binary-searches `inlays` for the first inlay at or after `edit.new.start` (`:656-664`);
  - re-inserts every inlay whose anchor still resolves inside the new range, skipping invalid anchors (`:666-683`);
  - emits an `InlayEdit` (`:692-695`).

  Cost is roughly O(E·log T + inlays in the edited ranges). When there are no inlays, edits pass straight through (`:603-625`).
- **Per splice (`:729-778`).** `retain(|i| !to_remove.contains(..))` is O(H·R) (`:738-745`). Each insert is `Vec::insert` after a binary search, so O(H) (`:754-763`). Empty inlays are dropped (`:748-751`). Every touched offset then goes through `sync` as a zero-width edit (`:768-777`).
- **Per frame.** A snapshot with no pending edits only bumps the version (`:591-602`). `chunks()` seeks once and streams (`:1248-1280`). For each inlay chunk it scans the inlay highlight maps (`:326-334`).
- **Linear scans.** Hover hit-testing walks every current inlay (`inlays/inlay_hints.rs:662-674`), and so does `apply_fetched_hints` (`:903-911`).

### 1.2 Coordinate spaces and the buffer ↔ display mapping

- **The stack.** `MultiBufferOffset/Point` → `InlayOffset/InlayPoint` → `FoldPoint` → `TabPoint` → `WrapPoint` → `BlockPoint` (= `DisplayPoint`). The module docs describe it (`display_map.rs:8-15`), `sync_through_wrap` chains it (`:560-570`), and `splice_inlays` does the same (`:1312-1334`).
  - Forward conversion: `point_to_display_point` (`:1675-1682`).
  - Reverse conversion: `display_point_to_inlay_point` (`:1761-1770`), then `to_buffer_point` (`:1740-1743`).
- **Only the InlayMap knows about hints.** The fold, tab and wrap layers see hint text as ordinary text:
  - Folds are mapped with `to_inlay_offset(start)..to_inlay_offset(end)` (`fold_map.rs:531-536`).
  - The wrap fuzz test mutates inlays (`wrap_map.rs:1907-1915`).
- **Columns are UTF-8 bytes.** `InlayPoint(pub Point)` rows and columns include inlay bytes.
- **Helpers that skip hint text:** `buffer_offset_to_inlay_ranges` (`inlay_map.rs:981-1024`), `BufferOffsetToInlayPointCursor` (`:1364-1431`), `isomorphic_display_point_ranges_for_buffer_range` (`display_map.rs:1687-1692`) and `contiguous_display_point_range_for_buffer_range` (`:1697-1721`).

### 1.3 Caret, click and selection

**Bias depends on the hint kind** (`lsp_command.rs:3902-3907`):

```rust
let position = snapshot.clip_point_utf16(point_from_lsp(lsp_hint.position), Bias::Left);
let position = if kind == Some(InlayHintKind::Parameter) {
    snapshot.anchor_before(position)
} else {
    snapshot.anchor_after(position)
};
```

**Canonical display position.** `to_inlay_point` is `InlayPointCursor::map(point, Bias::Left)` (`inlay_map.rs:976-979`). It walks past left-biased inlays and stops before right-biased ones (`:1332-1360`). `ToDisplayPoint for Point` also uses `Bias::Left` (`display_map.rs:2656-2660`). So:
- At a **type or other** hint's anchor, the caret renders **before (left of)** the hint: `let x‸: i32`.
- At a **parameter** hint's anchor, it renders **after (right of)** the hint: `foo(a: ‸1)`.
- Left-biased inlays sort before right-biased ones at the same offset (anchor `cmp` compares offset, then bias: `text/src/anchor.rs:91-103`). Test: `"abx|123||456|yDzefghi"` (`inlay_map.rs:1681-1702`).

**Each buffer position has exactly one display position.** `clip_point` (`:1044-1134`) moves any point inside an inlay, or on the wrong side of one, to a neighbour in the requested bias direction. For a right-biased inlay `|123|` at column 3 (`:1640-1647`):

```rust
inlay_snapshot.clip_point(InlayPoint::new(0, 4), Bias::Left)  == InlayPoint::new(0, 3)
inlay_snapshot.clip_point(InlayPoint::new(0, 4), Bias::Right) == InlayPoint::new(0, 9)
```

The fuzz test asserts that clipped points round-trip through the buffer (`:2157-2170`, `:2237-2253`). **The caret can never sit inside a hint.**

**Arrow keys.** `left` and `right` move one display column, then clip with `Bias::Left` or `Bias::Right` (`movement.rs:38-46`, `:67-75`). A hint is crossed in a single keypress. Word boundaries ignore inlays (test at `movement.rs:1234-1305`).

**Vertical movement.** The goal is a pixel x from `x_for_display_point` (`movement.rs:119-132`), so hint widths count toward it.

**Clicks.** `point_for_position` (`element.rs:10734-10783`) computes both clips and picks the side by the inlay's bias:

```rust
let nearest_valid = if previous_valid == next_valid {
    previous_valid
} else {
    match self.snapshot.inlay_bias_at(exact_unclipped) {
        Some(Bias::Left) => next_valid,
        Some(Bias::Right) => previous_valid,
        None => previous_valid,
    }
};
```

`mouse_left_down` then selects at `nearest_valid` (`element/mouse.rs:724-757`). Clicking anywhere on a hint puts the caret at the hint's anchor, on its canonical side.

**Selection painting.** Hints at either boundary are excluded from the highlight; hints between selected characters are included. `SelectionLayout::new` (`element.rs:285-297`) says why it moves the head:

> "Keep the cursor attached to the highlight boundary; the anchor-bias display position may sit on the far side of a boundary inlay the highlight excludes."

Test: `display_map.rs:4354-4492`.

### 1.4 Request strategy

**Triggers** (reasons at `inlay_hints.rs:286-296`):

| Reason | Where it fires |
|---|---|
| `BufferEdited` | on edit (`editor.rs:10083-10086`) |
| `NewLinesShown` | on scroll, behind a 50 ms timer (`editor.rs:11573-11599`); on excerpt updates (`:10123`); after folding or unfolding (`fold.rs:598`) |
| `RefreshRequested` | project event (`editor.rs:2105-2112`) |
| `ServerRemoved` | `editor.rs:2126` |
| `LanguageServerRegistered` | `editor.rs:2155-2162` |
| `SettingsChange` | `editor.rs:10428` |
| `ModifiersChanged` | `element.rs:865-886` |

**Range.** The editor takes the visible range: the scroll anchor plus `visible_line_count` display rows (`editor.rs:4090-4109`). It splits that range per buffer excerpt (`completions.rs:172-190`, `inlay_hints.rs:443-468`) and snaps it to fixed **50-row chunks** (`MAX_ROWS_IN_A_CHUNK = 50`, `lsp_store/inlay_hints.rs:109`). Each chunk is its own LSP request for `(start_row, 0)..(end_exclusive, 0)` (`row_chunk.rs:15-23`, `:102-119`). Chunk selection is lenient: a chunk whose exclusive end touches the range is also fetched (`row_chunk.rs:63-81`).

**Debounce.**
- Defaults (`language_settings.rs:461-471`): `edit_debounce_ms` 700 and `scroll_debounce_ms` 50. A value of 0 disables it (`editor.rs:1230-1236`).
- Selected at `inlay_hints.rs:358-370`:

  ```rust
  let debounce = match &reason {
      InlayHintRefreshReason::SettingsChange(_)
      | InlayHintRefreshReason::Toggle(_)
      | InlayHintRefreshReason::BuffersRemoved(_)
      | InlayHintRefreshReason::ModifiersChanged(_) => None,
      _may_need_lsp_call => self.inlay_hints.as_ref().and_then(|inlay_hints| {
          if invalidate_cache.should_invalidate() {
              inlay_hints.invalidate_debounce
          } else {
              inlay_hints.append_debounce
          }
      }),
  };
  ```

- The debounce is applied inside the spawned task (`:1065-1067`).

**`workspace/inlayHint/refresh`.**
- The client advertises `refresh_support: Some(true)` (`lsp/src/lsp.rs:908-910`).
- The handler (`lsp_store.rs:1119-1133`) calls `refresh_inlay_hints`, which marks the server pending in every buffer and emits an event (`lsp_store/inlay_hints.rs:347-376`).
- The first query afterwards drops that server's cache; concurrent queries share the re-fetch (`:243-266`, `lsp_store.rs:8676-8681`).
- Zed also refreshes when a server's work-done progress ends. That is rate-limited to once per server per buffer version, because rust-analyzer reports work after every save (`lsp_store.rs:12097-12111`, `lsp_store/inlay_hints.rs:176-188`).

**Version checks.**
- Editor level: `hint_chunk_fetching: (Global, HashSet<Range<BufferRow>>)`. When the buffer version moves on, fetched chunks and running tasks are cleared (`inlay_hints.rs:475-486`).
- Project level: `latest_lsp_data` resets the whole `BufferLspData` when the version moves on (`lsp_store.rs:14816-14829`).
- A fetch that completes for an old version returns empty rather than index the rebuilt chunk table (`:8763-8769`; test `inlay_hints.rs:2381-2501`).
- `known_chunks` is filtered by version (`lsp_store.rs:8690-8693`).

**Cache.** There are two levels:
1. Per buffer, per chunk, per server hints, with `hints_by_id` for resolve and shared in-flight fetch tasks (`lsp_store/inlay_hints.rs:80-89`, `:200-229`; `lsp_store.rs:8700-8820`).
2. The editor's `added_hints`, which records hint ids already on screen so cached results are not re-inserted (`inlay_hints.rs:1009-1016`).

**Invalidation.**
- `BufferEdited` invalidates the project cache for **every buffer of the same language** in the multibuffer (`inlay_hints.rs:386-416`).
- Identical hint text at the same position from different servers is deduplicated (`:983-1002`).

### 1.5 After an edit, before the new response

- **Hints follow their anchors.** `sync` recomputes each inlay's offset from its anchor (`inlay_map.rs:666-683`).
  - Text inserted at the anchor goes after a left-biased (parameter) hint and before a right-biased (type) hint. Test comment: "Edits ending where the inlay starts should not move it if it has a left bias" (`:1704-1716`).
- **A hint disappears if the character its anchor is attached to is deleted.** `Anchor::is_valid` requires the anchor's fragment to still be visible (`text/src/anchor.rs:150-167`), and `sync` skips invalid anchors (`inlay_map.rs:667-669`). Test comment: "An edit surrounding the inlay should invalidate it" (`:1667-1679`). The inlay stays in the `Vec` until the next splice removes it.
- **The project cache is dropped immediately** (`lsp_store.rs:14816-14829`; `invalidate_inlay_hints`, `:8651-8660`).
- **Old hints stay on screen until fresh results arrive**, which is the 700 ms debounce plus the round-trip. `apply_fetched_hints` then removes all visible hints for that buffer and inserts the new ones in one splice (`inlay_hints.rs:934-951`, `:1044`), so there is no flicker window. `InlaySplice` is documented as chosen "to help avoid extra hint flickering and 'jumps'" (`inlays.rs:29-32`).

### 1.6 Interactions

**Hover tooltips.**
- The flow: when the pointer is over a position that isn't valid text, `mouse_moved` calls `update_inlay_link_and_hover_points` (`element/mouse.rs:252-268`). That function finds the hint between the anchors of `previous_valid` and `next_valid` (`inlay_hints.rs:654-675`) and asks for the resolved hint (`:676-682`).
  - A `String` label with a tooltip calls `hover_at_inlay`, using a range that excludes padding (`:686-729`).
  - `LabelParts` use `find_hovered_hint_part` and per-part tooltips (`:730-791`; `hover_popover.rs:138-158`).
- The popover waits `hover_popover_delay` and is not re-shown for the same range (`hover_popover.rs:160-243`).

**Resolve.**
- Resolve is lazy and happens only on hover; `resolved_hint` has one caller (`inlay_hints.rs:680`).
- `resolved_hint` spawns the resolve and returns `None` (`lsp_store.rs:6823-6879`). On error the hint reverts to `CanResolve` (`:6857-6862`).
- `resolve_inlay_hint` (`lsp_store/inlay_hints.rs:280-345`) short-circuits when the server has no `resolveProvider` (`lsp_command.rs:4262-4276`).
- The tooltip appears on the next mouse move after the resolve lands; nothing re-triggers hover. The test calls the update twice (`hover_popover.rs:2639-2650`, `:2710-2720`).
- The cache is updated in place; the text on screen is not re-spliced.

**Cmd/Ctrl-click on a label part with a location.**
1. The part's `location` plus the secondary modifier calls `show_link_definition(TriggerPoint::InlayHint(highlight, location, server_id))` (`inlay_hints.rs:792-810`).
2. That produces a link state holding `HoverLink::LspLocation` and underlines the part through `highlight_inlays` (`hover_links.rs:629-632`, `:707-712`).
3. The click goes to `handle_click_hovered_link` (`element/mouse.rs:980`), then `navigate_to_hover_links`, whose `LspLocation` branch calls `compute_target_location` (`navigation.rs:1704-1723`).

**Plain click on a part with a `command`.**
- `HoveredInlayHintCommand` (`inlay_hints.rs:36-62`, `:749-766`) shows a pointing-hand cursor (`element.rs:5860-5872`).
- Mouse-down is swallowed (`element/mouse.rs:616-624`).
- A single unmodified click runs the command through `apply_code_action` (`inlay_hints.rs:839-864`, `element/mouse.rs:961-973`).

**textEdits (double-click to apply): not supported.**
- `project::InlayHint` has no `text_edits` field (`project.rs:527-535`).
- `project_to_lsp_hint` writes `text_edits: None` (`lsp_command.rs:4202`).
- The client still advertises `textEdits` as resolvable (`lsp/src/lsp.rs:1033-1041`).

**Toggling.**
- The `ToggleInlayHints` action (`actions.rs:899-900`, `inlay_hints.rs:325-335`) calls `LspInlayHintData::toggle` (`:117-127`). Disabling clears the display (`:546-557`).
- Settings changes diff the allowed kinds (`:168-253`).
- `toggle_on_modifiers_press` holds hints on or off while modifiers are held; `should_show = enabled != modifiers_override` (`:104-115`, `:276-282`).
- Nothing is requested while hints are disabled (`:610-616`).

### 1.7 Rendering

- **Style.**
  - `make_inlay_hints_style` uses the theme's syntax `hint` style, falls back to `status().hint`, and adds a background only with `show_background` (`editor.rs:584-610`). It is applied to `InlayId::Hint` chunks (`inlay_map.rs:345`) through `HighlightStyles { inlay_hint, edit_prediction }` (`display_map.rs:1403-1407`, `:1862`).
  - Hovered or linked parts get an extra highlight, split on UTF-8 boundaries (`inlay_map.rs:406-422`, `:440-452`).
- **Padding.** Real spaces are added to the inlay rope unless the label already starts or ends with one (`inlays.rs:60-75`; tests at `inlay_map.rs:1474-1578`, including a multibyte emoji case). The padding therefore takes the hint's background.
  - A hack drops left padding on type hints from `typescript-language-server` (`lsp_command.rs:4327-4339`).
- **No truncation.** Label parts are concatenated (`project.rs:851-858`).
- **Kinds.** Only `Type`, `Parameter` and `None` exist (`lsp_command.rs:3896-3900`). The per-kind `show_*` settings filter (`language_settings.rs:441-452`, `:481-494`; `inlay_hints.rs:1009-1016`), but every kind shares one style.
- **Details.**
  - Whitespace markers are not drawn inside inlays (`element.rs:7606`).
  - A diagnostic underline continues through inlays inside the diagnostic span (`display_map.rs:1867-1902`).
  - Translucent inlay colours are blended with the background (`:1880-1887`).

### 1.8 Edge cases guarded and suspected bugs

- **UTF-8 highlight split panic** (#33641): fixed with `ceil_char_boundary` (`inlay_map.rs:440-452`; tests `:2476-2663`).
- **Stale anchors after a path key is reused**: a "cannot summarize backward" crash (`:2358-2445`).
- **Out-of-range server positions**:
  - rows past the end are dropped (`lsp_command.rs:4354`);
  - columns are clipped to the line end (`:3902`);
  - a hint at EOF uses `Anchor::max`, which is always valid (`text/src/text.rs:2672-2675`).
  - Test: `inlay_hints.rs:5320-5368`.
- **Duplicates from a refresh racing a fetch**: a chunk response replaces that server's hints for the chunk (`lsp_store/inlay_hints.rs:208-215`; test `inlay_hints.rs:1391`).
- **Edit-then-scroll race** leaving a chunk permanently empty (`inlay_hints.rs:940-950`; test `:4643`).
- **Same-position hints** keep server order and are not deduplicated within one server (test `:4102-4268`). The splice binary search uses `.then(Less)` so new items go after equal ones (`inlay_map.rs:754-763`).
- **Folds.** Boundaries go through `to_inlay_offset`, which ignores the queried hint's bias (`fold_map.rs:531-536`, `inlay_map.rs:940-974`):
  - at a fold start, left-biased hints stay visible and right-biased ones fold away;
  - at a fold end it is the reverse.
  - Folds are fuzzed together with inlays (`fold_map.rs:2070-2130`).
- **Soft wrap**: no special case, so a hint can wrap mid-label (fuzz at `wrap_map.rs:1907-1915`).
- **Newlines in labels** are supported: they create display rows with `buffer_row: None` (`inlay_map.rs:1901-1945`). They are not sanitised.
- **(inferred) Parameter-hint label parts can't be hovered or clicked.**
  - The part hit-test uses `anchor_to_inlay_offset(hint.position)` (`display_map.rs:1750-1753`), which ignores bias. For a left-biased (parameter) inlay, `to_inlay_offset` returns the offset **after** the inlay (`inlay_map.rs:945-956`).
  - `find_hovered_hint_part` requires `hovered_offset >= hint_start` (`hover_popover.rs:143`), so the parts never match.
  - `HoveredInlayHintCommand::contains_point` has the same issue (`inlay_hints.rs:55-60`).
  - The hover and link tests only use `TYPE` hints (`hover_links.rs:1896`, and the hover_popover test at `hover_popover.rs:2560-2610`).
- **(inferred) Wrong part at shared positions.** For several hints at one position the hovered hint is chosen with `max_by_key(id)` (`inlay_hints.rs:674`), which can pick the wrong one.

---

## 2. Helix

### Key types (verbatim)

```rust
// helix/helix-core/src/text_annotations.rs:14-18, 67-71, 277-282
pub struct InlineAnnotation {
    pub text: Tendril,
    pub char_idx: usize,
}
pub struct Overlay {
    pub char_idx: usize,
    pub grapheme: Tendril,
}
pub struct TextAnnotations<'a> {
    inline_annotations: Vec<Layer<'a, InlineAnnotation, Option<Highlight>>>,
    overlays: Vec<Layer<'a, Overlay, Option<Highlight>>>,
    line_annotations: Vec<(Cell<usize>, RawBox<dyn LineAnnotation + 'a>)>,
}
// :115-174 pub trait LineAnnotation { fn reset_pos(..); fn skip_concealed_anchors(..); fn process_anchor(..);
//   fn insert_virtual_lines(&mut self, line_end_char_idx: usize, line_end_visual_pos: Position, doc_line: usize) -> Position; }

// helix/helix-core/src/doc_formatter.rs:30-40, 62-70
pub enum GraphemeSource {
    Document { codepoints: u32 },
    VirtualText { highlight: Option<Highlight> },
}
pub struct FormattedGrapheme<'a> {
    pub raw: Grapheme<'a>,
    pub source: GraphemeSource,
    pub visual_pos: Position,
    /// Document line at the start of the grapheme
    pub line_idx: usize,
    /// Document char position at the start of the grapheme
    pub char_idx: usize,
}

// helix/helix-view/src/document.rs:151, 160, 276-298, 321-326
pub(crate) inlay_hints: HashMap<ViewId, DocumentInlayHints>,
pub inlay_hints_oudated: bool,
pub struct DocumentInlayHints {
    pub id: DocumentInlayHintsId,
    pub type_inlay_hints: Vec<InlineAnnotation>,
    pub parameter_inlay_hints: Vec<InlineAnnotation>,
    pub other_inlay_hints: Vec<InlineAnnotation>,
    pub padding_before_inlay_hints: Vec<InlineAnnotation>,
    pub padding_after_inlay_hints: Vec<InlineAnnotation>,
}
pub struct DocumentInlayHintsId {
    pub first_line: usize,
    pub last_line: usize,
}
```

Signatures (whitespace collapsed):

```rust
pub fn visual_offset_from_block(text: RopeSlice, anchor: usize, pos: usize, text_fmt: &TextFormat, annotations: &TextAnnotations) -> (Position, usize)            // position.rs:149-155
pub fn char_idx_at_visual_offset(text: RopeSlice, mut anchor: usize, mut row_offset: isize, column: usize, text_fmt: &TextFormat, annotations: &TextAnnotations) -> (usize, usize) // :360-367
pub fn compute_inlay_hints_for_all_views(editor: &mut Editor, jobs: &mut crate::job::Jobs) // commands/lsp.rs:1332
pub enum Assoc { Before, After, AfterWord, BeforeWord, BeforeSticky, AfterSticky }        // transaction.rs:33-48
```

### 2.1 Data structure and complexity

- **Structure.**
  - Per (document, view) there are five sorted `Vec<InlineAnnotation>` keyed by char index (`document.rs:151`, `:276-298`). Splits keep their own sets.
  - Only hints for roughly three viewport heights are stored (`lsp.rs:1362-1372`).
- **Per edit.** Each vector is mapped with `ChangeSet::update_positions`. That is O(N+M) for sorted input and equivalent to calling `map_pos` on each position (`transaction.rs:374-388`; call sites `document.rs:1573-1598`).
- **Per frame.**
  - `View::text_annotations` borrows the vectors as layers every render (`view.rs:458-492`).
  - `reset_pos` binary-searches each layer (`text_annotations.rs:193-199`, `:295-301`).
  - Consuming an annotation is O(1) per grapheme (`:201-210`, `:373-381`).
  - The formatter restarts at the line holding the viewport anchor (`doc_formatter.rs:208-233`).
- **Hit tests and vertical moves** replay the formatter from the start of the line (`position.rs:149-169`, `:411-452`), so they cost O(chars from line start).

### 2.2 Coordinate spaces and the mapping

- **One persistent coordinate.** The only stored coordinate is the rope char index (`text_annotations.rs:11-18`). Visual `Position { row, col }` exists only while `DocumentFormatter` runs (`doc_formatter.rs:432-479`).
- **No display map.** The buffer ↔ display mapping is recomputed on demand:
  - char → visual: `visual_offset_from_block` and `visual_offset_from_anchor` (`position.rs:149-260`);
  - visual → char: `char_idx_at_visual_offset` and `char_idx_at_visual_block_offset` (`:360-452`).
  - The current annotations are passed to each call (`view.rs:563-592`).
- **Injection.** A virtual grapheme takes the anchor's `char_idx` and has `doc_chars() == 0`. It is emitted **before** the document grapheme at that index (`doc_formatter.rs:53-58`, `:235-275`, `:451-459`).
- **Ordering at one position.** The first layer with an annotation at the position wins, and each layer is drained before the next (`text_annotations.rs:373-381`). The layers are added in this order (`view.rs:484-492`):

  ```rust
  // Overlapping annotations are ignored apart from the first so the order here is not random:
  // types -> parameters -> others should hopefully be the "correct" order for most use cases,
  // with the padding coming before and after as expected.
  text_annotations
      .add_inline_annotations(padding_before_inlay_hints, None)
      .add_inline_annotations(type_inlay_hints, type_style)
      .add_inline_annotations(parameter_inlay_hints, parameter_style)
      .add_inline_annotations(other_inlay_hints, other_style)
      .add_inline_annotations(padding_after_inlay_hints, None);
  ```

  The comment is inaccurate: all overlapping annotations are shown, in layer order.

### 2.3 Caret, click and selection

- **Side at the anchor: always after (right of) the hint.** `visual_offset_from_block` returns the first grapheme whose `next_char_pos() > pos`. Virtual graphemes don't advance the position, so the result is the real grapheme after the hint (`position.rs:161-166`).
- **Cursor and selection never paint hint glyphs.** Both are overlay highlights (`ui/editor.rs:157`, `:531-560`), and virtual graphemes get `overlay_style: Style::default()` (`ui/document.rs:142-150`). A selection therefore shows a gap where a hint sits inside it.
- **The caret cannot be inside a hint.** Virtual text has no char index.
- **Horizontal movement** steps graphemes in the rope and ignores annotations (`movement.rs:34-53`). The block cursor jumps over the hint visually.
- **Clicks and vertical movement** resolve a column that lands on virtual text to the **last real grapheme to its left** on that row. If the row starts with virtual text, they use the next real grapheme (`position.rs:419-450`; doc comment `:336-345`):

  ```rust
  if grapheme.visual_pos.col + grapheme.width() > column {
      if !grapheme.is_virtual() {
          return (grapheme.char_idx, 0);
      } else if found_non_virtual_on_row {
          return (last_char_idx, 0);
      }
  } else if !grapheme.is_virtual() {
      found_non_virtual_on_row = true;
      last_char_idx = grapheme.char_idx;
  }
  ```

  So clicking `: i32` in `let x: i32` selects `x` (the anchor minus one), not the anchor.
- **Rows made only of virtual text** return the previous character plus a virtual-row offset (`position.rs:354-359`; test `:816-838`). `move_vertically_visual` adds 1 when moving forward past such a row (`movement.rs:84-94`).
- **The goal column is visual**, so hint widths count (`movement.rs:71-74`, `:121-125`).

### 2.4 Request strategy

- **Trigger.** Only the idle timer: 250 ms by default (`editor.rs:348`, `:1199`), reset on input (`ui/editor.rs:1187`, `:1490`). When it fires, `handle_idle_timeout` runs (`ui/editor.rs:1160-1163`) and computes hints for every view (`lsp.rs:1332-1346`). There is no other debounce, and the timer does not re-arm until the next input (`application.rs:686-689`, `editor.rs:1520-1525`).
- **Range.** Roughly one view height above to two below the first visible line (`lsp.rs:1366-1372`):

  ```rust
  let first_line = first_visible_line.saturating_sub(view_height);
  let last_line = first_visible_line
      .saturating_add(view_height.saturating_mul(2))
      .min(len_lines);
  ```

- **Skip rule.** The request is skipped if the document isn't marked outdated and the id (the line range) is unchanged (`:1374-1385`).
- **One server.** Only the first language server with the feature is asked (`:1355-1357`). Dynamic registration options are not recognised (`client.rs:391-394`, `:1248-1254`).
- **No refresh support.** The client sends `refresh_support: Some(false)` (`client.rs:604-606`).
- **No versioning.** The only state is the `inlay_hints_oudated` flag (`document.rs:158-160`). It is set on every change (`:1582`) and cleared when a response is applied (`lsp.rs:1421`, `:1512`).
- **No cache.** The latest response replaces the view's hints wholesale (`lsp.rs:1501-1512`, `document.rs:2412-2415`).
- **Resets.** Hints are cleared when an LSP stops (`typed.rs:1914-1919`, #10741), when the language is refreshed (`editor.rs:1811-1812`) and when the feature is disabled (`editor.rs:1900-1912`).
- **Config.** `display-inlay-hints` (default false) and `inlay-hints-length-limit` (`editor.rs:633-639`, `:656-658`; `book/src/editor.md:173-174`). Toggle with `:toggle-option` or `:set-option` (`typed.rs:2279`, `:2311`, `:3811-3825`), which goes through `refresh_config` and then `_refresh` (`editor.rs:1508-1512`).

### 2.5 After an edit, before the new response

- **Every hint is mapped with `Assoc::After`** (`document.rs:1574-1580`):

  ```rust
  changes.update_positions(annotations.iter_mut().map(|annotation| (&mut annotation.char_idx, Assoc::After)));
  ```

  | Edit | Effect on the hint |
  |---|---|
  | Insert at the hint's position | The hint moves after the inserted text |
  | Delete a range containing the hint | The hint collapses to the deletion start and is not removed (`transaction.rs:459-462`), so several stale hints can stack at one point |
  | Replace a range starting at the hint | The hint stays before the replacement (`:465-467`) |
  | Replace a range with the hint strictly inside | The hint moves after the replacement (`:469-476`) |

- **The old hints stay visible** until the next idle-time request completes.
- **(inferred) Race with no version check.**
  - The response is converted against the current text (`lsp.rs:1436-1446`) and then clears the outdated flag (`:1512`).
  - If the user edits while a request is in flight, and the response lands before the next idle tick, the idle tick sees "not outdated, same id" and skips (`:1379-1385`).
  - The misplaced hints then stay until the next edit or scroll.
- **(inferred) Typing at a parameter hint splits it off.** Typing at the start of an argument pushes the parameter hint right (`Assoc::After`), which shows `foo(2a: 1)` until a refresh.

### 2.6 Interactions

- **None.**
  - No resolve: `resolve_support: None` (`client.rs:726-729`).
  - No tooltips, no clickable parts and no textEdits. Labels are flattened to a string (`lsp.rs:1448-1455`).
- **Toggle** only through config (§2.4).

### 2.7 Rendering

- **Styles.** Theme keys `ui.virtual.inlay-hint`, `.type` and `.parameter` (`view.rs:479-482`; `book/src/themes.md:353-355`). Virtual text is patched onto `ui.text` (`ui/document.rs:142-150`).
- **Padding** is a separate single-space annotation with no highlight (`lsp.rs:1490-1498`, `view.rs:488`, `:492`). The hint theme's background therefore doesn't cover it.
  - **(inferred)** At one position, all before-padding comes first and all after-padding last. Two padded hints render as `␠AB␠`, not `A␠␠B`.
- **Truncation** by display width, grapheme-aware, with a trailing `…` (`lsp.rs:1456-1480`).
- **Whitespace and tabs** in virtual text draw as plain spaces or `virtual_tab` (`ui/document.rs:339-346`).
- **Soft wrap.** Virtual graphemes wrap like words (`doc_formatter.rs:359-420`; tests `doc_formatter/test.rs:176-189`).

### 2.8 Edge cases guarded

- Panic when the view anchor was out of bounds (#6883): fixed with `.min(doc_text.len_chars())` (`lsp.rs:1368`).
- Servers returning hints unsorted (`:1426-1428`).
- Unresolvable positions are skipped (`:1440-1446`). Lines past the end map to EOF and columns clip to the line end (`helix-lsp/src/lib.rs:146-158`, `:206-211`).
- The config changed or the view closed while a request was in flight (`lsp.rs:1402-1405`).
- The response is added to the document that was queried, not the one now focused (`:1407-1411`).
- An empty or `None` response clears the hints (`:1413-1423`).
- A virtual `\n` starts a new visual row without advancing the document line (`doc_formatter.rs:463-473`).
- There is no folding in this Helix snapshot.

---

## 3. Lapce / Floem

### Key types (verbatim)

```rust
// floem/src/views/editor/phantom_text.rs:14-28, 31-41, 48-51
pub struct PhantomText {
    /// The kind is currently used for sorting the phantom text on a line
    pub kind: PhantomTextKind,
    /// Column on the line that the phantom text should be displayed at
    pub col: usize,
    /// the affinity of cursor, e.g. for completion phantom text,
    /// we want the cursor always before the phantom text
    pub affinity: Option<CursorAffinity>,
    pub text: String,
    pub font_size: Option<usize>,
    // font_family: Option<FontFamily>,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub under_line: Option<Color>,
}
pub enum PhantomTextKind { Ime, Placeholder, Completion, InlayHint, Diagnostic }  // doc comments trimmed
pub struct PhantomTextLine {
    /// This uses a smallvec because most lines rarely have more than a couple phantom texts
    pub text: SmallVec<[PhantomText; 6]>,
}

// floem/editor-core/src/cursor.rs:168-173
pub enum CursorAffinity {
    /// `<: String>|`
    Forward,
    /// `|<: String>`
    Backward,
}

// floem/src/views/editor/layout.rs:22-31
pub struct TextLayoutLine {
    pub extra_style: Vec<LineExtraStyle>,
    pub text: TextLayout,
    pub whitespaces: Option<Vec<(char, (f64, f64))>>,
    pub indent: f64,
    pub phantom_text: PhantomTextLine,
}

// lapce/lapce-app/src/doc.rs:175-176
/// Inlay hints for the document
pub inlay_hints: RwSignal<Option<Spans<InlayHint>>>,
```

The `PhantomTextLine` methods (`phantom_text.rs:55-188`, verbatim bodies):

```rust
pub fn col_at(&self, pre_col: usize) -> usize {            // phantom at == col counts as "before"
    let mut last = pre_col;
    for (col_shift, size, col, _) in self.offset_size_iter() {
        if pre_col >= col { last = pre_col + col_shift + size; }
    }
    last
}
pub fn col_after(&self, pre_col: usize, before_cursor: bool) -> usize {
    let mut last = pre_col;
    for (col_shift, size, col, text) in self.offset_size_iter() {
        let before_cursor = match text.affinity {
            Some(CursorAffinity::Forward) => true,
            Some(CursorAffinity::Backward) => false,
            None => before_cursor,
        };
        if pre_col > col || (pre_col == col && before_cursor) {
            last = pre_col + col_shift + size;
        }
    }
    last
}
pub fn before_col(&self, col: usize) -> usize {
    let mut last = col;
    for (col_shift, size, hint_col, _) in self.offset_size_iter() {
        let shifted_start = hint_col + col_shift;
        let shifted_end = shifted_start + size;
        if col >= shifted_start {
            if col >= shifted_end { last = col - col_shift - size; } else { last = hint_col; }
        }
    }
    last
}
pub fn combine_with_text<'a>(&self, text: &'a str) -> Cow<'a, str>   // :148-168, inserts each phantom at col+shift; bails if `text.get(location..)` is None
pub fn offset_size_iter(&self) -> impl Iterator<Item = (usize, usize, usize, &PhantomText)> + '_  // (pre_col_shift, size, phantom.col, phantom)
```

`col_after_force` (`:91-100`) is the same as `col_after` but ignores `PhantomText.affinity`.

### 3.1 Data structure and complexity

- **Storage.** A xi-rope `Spans<InlayHint>` keyed by **byte offsets**. Each hint is stored as a one-byte interval `[offset, offset+1)` (`doc.rs:1038-1045`). The hints are sorted first because `SpansBuilder` requires order (`:1033-1037`).
- **Per edit.** `hints.apply_shape(delta)` (`doc.rs:769-776`). Upstream xi-rope rebuilds the tree from the delta's Copy/Insert elements; subtrees fully inside a Copy are shared as Arc clones, and leaf spans are intersected with the copied interval, with empty intersections dropped.
- **Every edit also sends a new whole-document request** from `on_update` (`doc.rs:646-663`).
- **Per response.** The Spans are rebuilt for the whole document, then `clear_text_cache` bumps `cache_rev` (`doc.rs:1024-1028`, `:815-825`), which invalidates every cached line layout (`mod.rs:1255-1260`).
- **Per layout miss.**
  - `phantom_text(line)` runs `iter_chunks`, classifies neighbouring characters for each hint, adds error-lens, completion and IME entries, and sorts (`doc.rs:1621-1853`).
  - `combine_with_text` does one `insert_str` per phantom, so O(len·k) (`phantom_text.rs:147-168`).
- **Uncached work.**
  - Lapce doesn't override `before_phantom_col`, so every call rebuilds `phantom_text` (`floem/src/views/editor/text.rs:216-225`; used throughout `visual_line.rs`, e.g. `:866`, `:1119-1127`).
  - `has_multiline_phantom` always returns true (`doc.rs:1855-1858`, "TODO: actually check"). That forces the non-linear visual-line path (`visual_line.rs:354-366`).

### 3.2 Coordinate spaces and the mapping

- **Coordinates:**
  - buffer byte offset;
  - `(line, col)`, with col as a byte offset within the line;
  - **layout index**: the byte index into the line string with phantom text spliced in;
  - `VLine` / `RVLine` visual lines (`visual_line.rs:1-33`);
  - pixels.
- **The mapping is per line only.** It lives in `PhantomTextLine`, stored on each `TextLayoutLine` (`layout.rs:22-31`):
  - `col_after` / `col_after_force` / `col_at` convert buffer col → layout index;
  - `before_col` converts layout index → buffer col.
- **Layout build.** `new_text_layout` replaces the trailing `\n` with a space (and `\r\n` with two) so a hint at the end of a line has somewhere to sit (`mod.rs:1368-1376`). It then calls `combine_with_text` and adds attribute spans for the phantom colour and size (`:1378-1413`).
- **Wrapping** is done by the text layout itself (`mod.rs:1418-1433`).

### 3.3 Caret, click and selection

- **Where the caret renders.** `line_point_of_line_col` (`mod.rs:988-1012`) calls `col_after(col, affinity == CursorAffinity::Forward)`. If the phantom has its own `affinity`, it overrides the cursor's; otherwise the cursor's affinity decides.
- **Cursor affinity comes from movement direction.** Moving left sets `Forward` and moving right sets `Backward` (`movement.rs:231-281`). A hint with no affinity therefore keeps the caret on the side it arrived from (documented at `cursor.rs:150-165`). One buffer offset can have **two** caret positions.
- **Lapce picks each hint's affinity from its neighbours** (`doc.rs:1655-1685`):

  ```rust
  let mut affinity = None;
  if let Some(prev_char) = prev_char {
      let c = get_char_property(prev_char);
      if c == CharClassification::Other {
          affinity = Some(CursorAffinity::Backward)
      } else if matches!(c, CharClassification::Lf | CharClassification::Cr | CharClassification::Space) {
          affinity = Some(CursorAffinity::Forward)
      }
  };
  if affinity.is_none() {
      if let Some(next_char) = next_char {
          let c = get_char_property(next_char);
          if c == CharClassification::Other {
              affinity = Some(CursorAffinity::Forward)
          } else if matches!(c, CharClassification::Lf | CharClassification::Cr | CharClassification::Space) {
              affinity = Some(CursorAffinity::Backward)
          }
      }
  }
  ```

  `Other` means letters and all non-ASCII (`floem/editor-core/src/word.rs:10-21`). What this produces:

  | Case | Affinity | Caret |
  |---|---|---|
  | `let x` + type hint | Backward | before the hint |
  | `foo(` + param hint + `1` | Forward | after the hint |
  | `foo(1, ` + param hint | Forward | after the hint |
  | `)` + end-of-line hint | Backward | before the hint |
  | Punctuation on both sides | None | falls back to travel direction |

- **The caret cannot be inside a hint.** `before_col` collapses the whole phantom to `hint_col` (`phantom_text.rs:130-145`).
- **Clicks.** `line_col_of_point` hit-tests the layout, then calls `before_col` (`mod.rs:1086-1166`, "We have to unapply the phantom text shifting…" at `:1138-1140`). Floem HEAD also returns the hit affinity, and `single_click` stores it (`mod.rs:575-595`). Clicking anywhere on a hint places the caret at the anchor; the hint's own affinity usually decides the side.
- **Selection painting.**
  - The left edge uses `Forward` and the right edge `Backward`, both forced (`view.rs:506-519`). Boundary hints are excluded and inner hints are covered.
  - The block cursor covers only the real character (`view.rs:1287-1305`).
  - Empty wrapped lines made only of phantom text get special handling (`view.rs:529-537`, `visual_line.rs:1520-1524`).
- **Vertical movement** uses a pixel `ColPosition::Col(x)` that includes phantom widths, then hit-tests and calls `before_col` on the target line (`movement.rs:318-361`, `mod.rs:1192-1238`).

### 3.4 Request strategy

- **Range: the whole document**, `(0,0)..end` (`lapce-proxy/src/dispatch.rs:614-627`).
- **Servers.** The request goes to all plugins; the first successful response wins and errors surface only if every plugin fails (`plugin/mod.rs:381-430`, `:846-869`).
- **Triggers.**
  - Every `on_update`: each edit batch, reload and initial load (`doc.rs:441-456`, `:478-487`, `:597-615`, `:646-663`).
  - Server status OK (`window_tab.rs:2150-2163`).
- **No debounce.**
- **Versioning.** The revision is captured at request time and the response dropped if the buffer has moved on (`doc.rs:1019-1028`).
- **No refresh or resolve.** The capabilities are default (`plugin/mod.rs:1715-1717`) and there is no refresh handler.
- **No cache** beyond the single `Spans`.
- **Requests continue while hints are disabled.** `get_inlay_hints` never checks the config (`doc.rs:1006-1044`).

### 3.5 After an edit, before the new response

- **Hints shift with `apply_shape`.** Because each span is one byte starting at the anchor:
  - text inserted at the anchor pushes the hint right (it is attached to the next character);
  - deleting or replacing that character **drops** the hint (upstream `push_maybe_split` drops empty intersections).
- **(inferred) Typing at a parameter hint splits it off**, like Helix: `foo(2a: 1)`.
- **Continuous typing freezes the hints.** Every response for an older revision is discarded (`doc.rs:1025`), so hints update only once a round-trip completes with no edits in between.
- **Stored LSP positions go stale.** `InlayHint.position` keeps the server's coordinates and is never updated. Ctrl-click still reads it (§3.6).

### 3.6 Interactions

- **Hover.** There is no hint-specific tooltip. Hovering a hint resolves to the anchor offset and requests an ordinary LSP hover at `prev_code_boundary(offset)` (`editor.rs:2821-2866`).
- **Ctrl/Cmd-click** (`editor.rs:2710-2745`):
  - `find_hint` (`:2753-2783`, `:3884-3920`) walks the hints on the line; a label part with a `location` sends `JumpToLocation`.
  - A part without a location does nothing, and a click off any hint falls back to `GotoDefinition`.
  - The enum it uses:

    ```rust
    enum FindHintRs { NoMatchBreak, NoMatchContinue { pre_hint_len: u32 }, MatchWithoutLocation, Match(Location) }
    ```

  - **(inferred) The hit-test is often wrong.** It compares `hint.position.character` (UTF-16 and stale after edits) with a UTF-8 layout byte index, counts only inlay-hint lengths, and ignores error-lens, completion and IME phantoms.
- **No resolve, commands or textEdits.**
- **Toggle.**
  - The `ToggleInlayHints` command is a no-op: `ToggleInlayHints => {}` (`window_tab.rs:928`).
  - The real switch is the `enable-inlay-hints` setting (default true; `config/editor.rs:148-150`, `defaults/settings.toml:42-44`), read when phantom text is built (`doc.rs:1636-1641`).

### 3.7 Rendering

- **Colours.** fg is `inlay_hint.foreground` and bg is `inlay_hint.background`; the dark theme uses `$text` and `#528abF37` (`doc.rs:1699-1703`, `defaults/dark-theme.toml:133-134`).
- **Background painting.** The background is drawn as extra-style rectangles for each layout run, so a wrapped hint gets several (`doc.rs:2016-2044`, `:2190-2239`; `floem view.rs:821-846`).
- **Font size.** `inlay_hint_font_size()` falls back to the editor size when outside `[5, editor size]` (`config/editor.rs:265-273`), and the layout applies `min(phantom, editor)` (`mod.rs:1405-1407`). The font-family setting is unused (commented out at `doc.rs:1701`).
- **Ignored or absent:** LSP padding (not read anywhere in `lapce-app`), truncation, kind-based styling. Label parts are joined (`doc.rs:1689-1694`).
- **Ordering at one column.** Sorted by (col, kind), so IME < placeholder < completion < inlay hint < diagnostic; the sort is stable (`doc.rs:1844-1852`, `phantom_text.rs:30-41`).

### 3.8 Edge cases guarded and suspected bugs

- A bad phantom location (past the end or not a char boundary) stops the splice. The remaining phantoms on that line are silently dropped (`phantom_text.rs:155-158`).
- **Multi-line phantom text** creates extra visual lines inside one buffer line (`visual_line.rs:2377-2470`, `"greet\nworld"`).
- **Wrapped phantom text** maps both visual lines to the same offset (`:2723-2800`). A short phantom stays joined to its neighbour: `"ahi "` (`:3015-3060`).
- Whitespace rendering accounts for phantom shifts (`mod.rs:1310-1322`).
- **(inferred) Hints exactly at EOF never show.** They are stored as an empty interval (`doc.rs:1042`), and the per-line filter `interval.start < end_offset` fails because `offset_of_line(last+1) == len` (`doc.rs:1645-1647`; `floem/editor-core/src/buffer/rope_text.rs:25-29`).
- **(inferred) A `null` result leaves stale hints.** The response type is `Vec<InlayHint>` (`plugin/mod.rs:846-853`), so an LSP `null` fails to deserialise and is ignored.
- LSP folding is commented out (`doc.rs:1091-1125`), so there is no interplay with folds.

---

## 4. Comparison

| Aspect | Zed | Helix | Lapce / Floem |
|---|---|---|---|
| Storage | Sorted `Vec<Inlay>` (anchors) plus `SumTree<Transform>` (`inlay_map.rs:34-58`) | Per (document, view): 5 sorted `Vec<InlineAnnotation>` by char index (`document.rs:276-298`) | `Spans<InlayHint>` with 1-byte intervals (`doc.rs:176`, `:1038-1045`), plus a per-line `PhantomTextLine` |
| Position model | CRDT `Anchor` with Bias: Parameter = Left, others Right (`lsp_command.rs:3902-3907`) | Char index remapped with `Assoc::After` (`document.rs:1574-1580`) | Byte offset, shifted by `apply_shape` |
| Display mapping | Persistent layered snapshot: inlay → fold → tab → wrap → block | Recomputed by the formatter on each query (`position.rs`) | Per line: splice into the layout string, `col_after` / `before_col` |
| Cost per edit | O(E·log T + inlays in range); splice O(H·R) | O(N+M) | `apply_shape` plus a full-document LSP request |
| Cost per frame | Visible chunks, O(log T) seek | Formatter from the line start; O(log n) per layer | Cached layouts; a miss rebuilds the line's phantoms. `before_phantom_col` is uncached |
| Request range | Visible range in 50-row chunks, per excerpt | ~3× view height around the viewport | Whole document |
| Debounce | Edit 700 ms, scroll 50 ms (+50 ms timer) | Idle timeout 250 ms | None |
| `inlayHint/refresh` | Yes, plus a rate-limited refresh when server work ends | No (`refresh_support: false`) | No |
| Version checks | Global version at editor level, project level and fetch completion | None (line-range id and an outdated flag) | Revision check, drop if stale |
| Cache | Per chunk and server, with hint ids; editor `added_hints` | None | None |
| After an edit | Follow anchors; hidden if the anchored char is deleted; atomic swap later | Shift; deleted ones collapse and stack; replaced wholesale later | Shift; dropped if the anchored char is deleted; replaced when a response arrives unedited |
| Caret side at anchor | Type/other: before. Parameter: after | Always after | Hint's heuristic affinity, else travel direction |
| Caret inside a hint | No (`clip_point`) | No | No (`before_col`) |
| Click on a hint | Caret at the anchor, canonical side (`nearest_valid`) | Caret on the real character left of the hint | Caret at the anchor, side from affinity |
| Arrow keys | One press crosses the hint | Grapheme steps; hint not counted | One press; side is sticky |
| Selection over hints | Boundary hints excluded, inner included; head snaps to the edge | Hints never highlighted | Boundary excluded, inner included |
| Hover tooltip | Yes: hint and per part; markdown or plain | No | No (ordinary hover at the anchor) |
| Resolve | Lazy on hover (`ResolveState`) | No | No |
| Clickable location | Cmd/Ctrl-click, underline highlight | No | Ctrl-click with a flawed hit-test |
| Label-part commands | Plain click runs `lsp::Command` | No | No |
| textEdits | No (field dropped) | No | No |
| Toggle | Action, settings, per-kind, hold-modifier | Config (`display-inlay-hints`) | Config only; command is a no-op |
| Padding | Spaces inside the label, styled | Separate unstyled annotations | Ignored |
| Truncation | None | Width limit + `…` | None (smaller font option) |
| Per-kind styling | No (filter only) | Yes (3 theme keys) | No |
| Newlines in label | Supported (new rows) | Supported (virtual row break) | Supported (multi-line phantom) |
| Soft wrap | Wraps as text | Wraps as words | Layout wraps it; bg per run |
| Folding | Bias-dependent at fold edges; hidden inside | No folding | Folding disabled |
| Multiple servers | Yes, deduplicated across servers | First server only | First successful response only |

---

## 5. Edge cases to reproduce and design lessons for scrive

**Edge cases worth covering with tests:**

1. **Hints at end of line and at EOF.** Zed anchors EOF to `Anchor::max` (`text/src/text.rs:2672-2675`). Lapce silently drops EOF hints. Floem makes space for end-of-line hints by replacing the newline with a space (`mod.rs:1368-1376`).
2. **Several hints at one position.** Keep server order with a stable sort and insertion after equal keys (`inlay_map.rs:754-763`; test `inlay_hints.rs:4102-4268`). Keep each hint's padding attached to it; Helix's padding layers reorder it.
3. **Deleted anchor text.** Hide or drop the hint, as Zed and Lapce do. Collapsing it like Helix stacks stale hints.
4. **Typing at the hint position.** Bias by kind: parameter hints stick to the text on their left, type and other hints to the text on their right. This keeps typed text on the side the caret renders. Without it you get Helix's and Lapce's `foo(2a: 1)` split.
5. **Out-of-range server positions.** Drop rows past the end, clamp columns to the line end, and convert UTF-16 (`lsp_command.rs:3902`, `:4354`).
6. **Stale responses.** Tag each request with the buffer version; discard or re-map on mismatch (Zed `lsp_store.rs:8763-8769`, Lapce `doc.rs:1025`). Helix lacks this.
7. **Chunk bookkeeping races.** An edit task racing a scroll task can leave a chunk marked as fetched but empty (`inlay_hints.rs:940-950`). A refresh racing a fetch can duplicate hints (`lsp_store/inlay_hints.rs:208-215`).
8. **UTF-8 boundaries** when splitting hint labels for highlights (#33641, `inlay_map.rs:440-452`).
9. **Folds.** Decide which hints stay visible at a fold's boundaries (Zed uses bias). Hints inside a fold are hidden but still cached.
10. **Soft wrap.** A hint may wrap mid-label. Paint backgrounds per layout run (`doc.rs:2190-2239`). Rows made only of hint text need defined vertical-movement behaviour (`position.rs:354-359`, `:816-838`).
11. **Newlines in labels.** Support them as extra rows (all three do) or sanitise them. Decide explicitly.
12. **Selection edges.** Exclude hints at the boundaries, include inner ones, and snap the head to the highlight edge (`element.rs:285-297`).

**Design lessons:**

1. **One canonical display position per buffer position** (Zed) keeps caret, selection and IME logic simple. Floem's two-sided affinity allows sticky sides but multiplies the edge cases. If you adopt affinity, derive the default side from the hint kind, and use Lapce's neighbouring-character heuristic (`doc.rs:1655-1685`) only when kind is `None`.
2. **Separate the model (anchored hints) from the display map.** For an iced widget, Floem's per-line splice (a phantom list with `col_after` / `before_col`, shaped together with the line) is the cheapest path to correct shaping and wrapping. Cache the per-line phantom data; don't recompute it on every coordinate query as Lapce's `before_phantom_col` does.
3. **Ask for the viewport, not the file.**
   - Split requests into fixed row chunks so scrolling adds instead of re-requesting.
   - Debounce edits (~700 ms) more than scrolling (~50 ms).
   - Swap old for new hints in one step so nothing flickers.
4. **Handle `workspace/inlayHint/refresh`.** It is cheap. Also consider a rate-limited refresh when server progress ends (`lsp_store/inlay_hints.rs:176-188`).
5. **Store resolved data by stable hint id.**
   - Resolve lazily on hover.
   - Hit-test label parts against the hint's actual display start; Zed's bias-agnostic start looks wrong for parameter hints.
   - Exclude padding from hover ranges.
   - Convert LSP positions to anchors once, on receipt. Lapce's reuse of raw LSP positions is its bug.
6. **Render padding as part of the hint** so background and hover treat it consistently. Offer a length limit with `…` (Helix). Skip whitespace markers inside hints (`element.rs:7606`), and keep hint text out of word motions and search.
7. **Interaction plan in priority order:** tooltip (with resolve), Cmd/Ctrl-click on locations, then plain-click commands. Zed has all three; nobody applies `textEdits` on double-click, so that would be new ground.

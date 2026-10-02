# Phase agent dispatch rules

Read this first. It combines MAP_PLAN.md Appendix A with this session's overrides. The **overrides win**
wherever they conflict with Appendix A or with a phase doc.

## Reading order (fully, before writing code)

1. `~/.claude/guides/RUST_STYLE.md` and `~/.claude/guides/OPAQUE.md`
2. The `/iced` skill (Skill tool, `iced`) — required for any phase touching iced-facing code
   (scrive-iced widget, glue, example). Its conventions govern that code.
3. The `/commit-and-comment` skill (Skill tool, `commit-and-comment`) — it governs every comment you
   write (see override 1).
4. `.claude/map/inlay-hints/MAP_PLAN.md` — "Current state", "Key design decisions", "Constraints",
   "Risks".
5. `.claude/map/inlay-hints/RESOLUTIONS.md` — binding answers; it wins over a phase doc.
6. Your phase doc `.claude/map/inlay-hints/MAP_PHASE_<N>.md`.

Ground truth is source, not memory. Read every file before you change it. Read lsp-types 0.97 from
`~/.cargo/registry/src/*/lsp-types-0.97.0/`, and iced from the pinned checkout under
`~/.cargo/git/checkouts/`. Phase docs cite line numbers from HEAD `8e72665`. Earlier phases have
moved code since, so locate sites by function or arm name.

## Override 1 — comments follow /commit-and-comment

This replaces Appendix A's "comments make up about 25% of the lines" guidance and any comment text
quoted in phase docs.

- **Write no comment by default.** Reach for a better name, an extracted function or a type first.
- **Doc comments on public items** (they are required by `#![deny(missing_docs)]`) say what the item
  is for, its inputs and outputs, and its contract. Never describe the implementation.
- **Any other comment** is a sentence or two for a domain expert, explaining a non-obvious *why*: a
  workaround, an ordering constraint, a spec rule (cite the LSP spec section), a measured constant.
- **Never narrate history or the plan.** No "previously", "now", "was changed", "Phase N", "D12",
  "per the plan", and no rejected alternatives.
- Phase docs quote suggested doc comments. Treat them as content hints, and trim them to these rules.
- Every `#[allow]` still carries a one-line justification. Tests keep a short `///` doc stating the
  invariant, plus string assert messages.

## Override 2 — commit checkpoints (you still never commit)

The orchestrator turns your work into a short ordered series of commits. Each commit must build and
pass tests on its own. Your phase dispatch names the **commit boundaries**. Implement them in that
order.

At each boundary:

1. Make the tree green:
   - `cargo clippy --workspace --all-targets -- -D warnings`;
   - the same with `--all-features` (every phase);
   - the affected crates' tests.
2. Save a cumulative patch against the phase's base commit:
   `git diff <BASE> -- . ':(exclude).claude' > .claude/map/inlay-hints/patches/phase<N>-<k>.patch`.
   `<BASE>` is given in your dispatch. The patch must include **new untracked files**, so run
   `git add -N <new files>` before `git diff`. `git add -N` is allowed; it only marks intent.
3. Write the proposed commit subject and a 1-3 line "why" body to
   `.claude/map/inlay-hints/patches/phase<N>-<k>.msg`:
   - Conventional Commits;
   - scopes used in this repo: `core`, `iced`, `lsp`, `examples`, `ci`, `docs`, `find`;
   - mark a breaking API change with `!`;
   - no AI attribution;
   - the body says why, and never narrates the diff.

If a boundary can't be made green alone, merge it with the next one. Record that in your report.
Don't commit red.

## Allowed commands

- `cargo build`, `cargo test`
- `cargo clippy --workspace --all-targets [--all-features] -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace [--all-features]`
- `cargo build --workspace --all-targets [--all-features] --target wasm32-unknown-unknown`
- `rustfmt --edition 2021 <file you created>`
- read-only git and grep: `git diff`, `git show`, `git status`, `git add -N`, `cargo tree`

## Forbidden commands

- `cargo run --example …` — it opens a GUI and hangs.
- `cargo fmt` — the code isn't rustfmt-clean.
- `git commit`, `git push`, `git stash`, `git checkout -- …`, `git reset`, `git restore` — the
  orchestrator commits.
- `cargo update`, and any edit to the iced pin.
- `cargo add` of a crate outside the plan's budget.

## Self-review before reporting

Check for:
- composite names on new scrive-lsp types;
- `use foo as bar`;
- `unwrap()` in library code;
- wildcard arms over enums you own;
- missing docs;
- comments that break override 1;
- `std::time`, threads or I/O in scrive-lsp;
- `Widget::new` where an iced helper exists;
- `#[allow]` without a justification;
- dead code;
- raw `bool` parameters;
- a missing `#[must_use]` where messages are returned;
- public fields on the invariant types listed in Constraints.

## Forbidden patterns

- traits over closed sets;
- shims, except the re-exports RESOLUTIONS.md specifies (R26);
- `todo!()`;
- speculative helpers or variants;
- `#[allow(dead_code)]`;
- AI attribution.

## Report

**If a real design decision isn't covered by the plan, STOP and report it** rather than inventing an
answer.

Otherwise, report:
- the patches and messages written (paths);
- what changed, file by file;
- the commands you ran and their results;
- what you couldn't verify;
- any "Decision:" you made;
- anything you think the plan or doc got wrong.

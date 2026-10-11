# Claude Instructions for jackerymon

## Project

Rust library, logging daemon, TUI, Prometheus exporter for Jackery
units over BLE.  IP/MQTT was ruled out for the Explorer; see `FINDINGS.md`.

- `PLAN.md` records why things are the way they are.  Read it before
  proposing architectural changes, and update it when a decision
  changes.
- Vendor manuals go in `third_party/`.  They are not ours; update
  `third_party/NOTICE` accordingly.

## Critical Rules

- Be extremely concise; sacrifice grammar for concision.
- Use built-in tools for file operations.
- Use globs for file search, grep for content search, read for viewing files.
- Do not request grep/sed/fd/find/ls/cat or similar CLI tools when you
  already have these capabilities built-in.
- Read code before modifying it.  Understand existing patterns and
  context before proposing changes.
- Always list unresolved questions at end.
- Write US English -- behavior, center, analog, initialize, labeled
  -- in docs, comments and anything a person reads.  Text quoted from
  a manual or another source keeps its own spelling.
- Keep documentation (.md files) up to date with code changes, in the
  same commit as the change.  This means all of them: `README.md`,
  `PLAN.md` and everything in `docs/`.  A decision that is reversed, a
  phase that is finished, or a figure that is retracted is a
  documentation change as much as a code one.


## Revision Control

- Do not add Claude attribution to commit messages.
- Do not commit without permission.
- PRs should generally be comprised of one functional change; suggest
  making a commit before moving onto something unrelated.
- All tests must pass before committing.
- Never use -a to commit; always enumerate the files.


## Programming Rules

- Prefer ASCII in code, in machine-facing output, and in anything a
  script might parse: logs, CLI output, error messages.  Ask before
  using Unicode there.
- Output meant for a person to read is the exception, and Unicode is
  fine in it with no ASCII fallback.  That covers the TUI -- box
  drawing, block elements, braille -- and the browser views, where a
  chart axis says the thing it means: tau, sigma, degrees, micro.
  One fallback existed and was dropped: ratatui draws chart axes with
  box-drawing glyphs a caller cannot replace, so the mode could never
  have been complete, and a fallback that is wrong in the one place it
  is needed is worse than none.
- Prefer consistency above most other concerns.
- Do not add trivial, obvious or redundant comments.
- Be DRY.
- Avoid magic constants.
- Only comment unintuitive or hard to understand code.
- Always comment data structures.
- Don't abbreviate by dropping letters from the middle of a word.
  Truncation (cutting from the end) is OK: `repeater` can shorten
  to `rep` or `repeat` but not to `rpt`.  Domain acronyms / wire-
  protocol terms are fine -- `scpi`, `gps`, `gpsdo`, `ocxo`, `efc`,
  `tfom`, `ffom`, `prn`, `pps`, `ti`, `utc`, `tai`, `rs232`, `uart`,
  `rx`, `tx`, `led`, `idn`, `tcode`, `emangle`, `adelay`.


## Rust Rules

- Use the latest stable Rust edition.
- Always run `cargo fmt` after changes and before commits.
- Always run `cargo clippy` after major changes and always before commits.
- Run tests with `cargo test`.
- Always use the narrowest visibility possible.
- Avoid public by default.
- Prefer `#[expect(lint, reason = "...")]` over `#[allow(lint)]`. If
  you must use `allow`, add `// [TODO] @<developer>: fix allow lint`.
- Use item-level imports, not nested crate/module imports.
- Prefer `use` statements at module top over inline imports.
- Avoid mutable variables when possible.  Prefer new bindings or shadows.
- Never re-export.  Use individual use statements where needed instead
  of `pub use` or `pub(crate) use`.
- Never employ a wildcard `use` statement on an enum when trying to
  shorten match arms.
- Use the newtype idiom as appropriate.
- Do not use `anyhow` in library crates at all; use typed errors via
  `thiserror` instead.  `anyhow` is for binaries only.
- Do not use `unsafe` without asking.
- When adding dependencies, use `cargo add` to ensure we install the
  latest version of dependencies.


## Working style

- **Adversarial review**: when a new idea/direction lands, run an
  adversarial review pass on it (what breaks, what it costs, what it
  forecloses) before/while implementing, and say so plainly.
- **Refactor first**: when working on anything significant, make a
  refactor pass first and commit that first.  If in doubt if a
  refactor is required, ask.

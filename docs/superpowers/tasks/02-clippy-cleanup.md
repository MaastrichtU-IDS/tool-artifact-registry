# 02 — Clear the clippy warnings

**Kind:** Bounded · **Source:** CI's advisory `cargo clippy` step (`.github/workflows/ci.yml`)

## Current warnings (`cargo clippy --all-targets`, 2026-09-23)

- `enclosing Ok and ? are unneeded` — `src/api/artifacts.rs:156`, `deref.rs:201`,
  `instances.rs:124`, `software.rs:159`, `runs.rs:80`
- `format!` in `format!` args — `src/api/instances.rs:712`
- `and_then(|x| Some(y))` → `map` — `src/api/openlineage.rs:193`, `src/domain/content.rs:439`
- manual `Iterator::find` — `src/model.rs:131`
- items after a test module — `src/auth/jwt.rs:498`, `src/mcp/transport.rs:376`
- field assignment outside initializer — `src/ratelimit.rs:819` (test code)
- too many arguments (8/7) — `src/ops/mod.rs:87`
- `iter().any()` → `contains()`, and `assert_eq!` with a literal bool — in tests
  (`tests/api.rs:1657`, `tests/mcp.rs:667`)
- dead code in `tests/common/mod.rs` (`CountingStore`, `Calls`) — used by some test binaries and
  not others, so each binary that does not use it warns.

Line numbers drift; re-run clippy rather than trusting them.

## Done looks like

- `cargo clippy --all-targets` prints no warnings.
- No behaviour change. `cargo test` all green, same test count as before.
- Judgement calls, not mechanical fixes:
  - `ops/mod.rs:87` (8 arguments): prefer a small params struct if it reads better; a targeted
    `#[allow(clippy::too_many_arguments)]` with a one-line reason is acceptable if it does not.
  - `tests/common/mod.rs`: `#[allow(dead_code)]` on the items, with a comment that each test
    binary compiles `common` separately. Do not delete them — `tests/api.rs` uses them.
- Optionally, if the user agrees: make the CI clippy step blocking (`-D warnings`) once clean.
  Ask first; it changes what can merge.
- One commit, or one per kind of fix. `cargo fmt` after.

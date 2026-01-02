# Repository Guidelines

## Project Structure & Module Organization
- `src/` contains the Rust CLI, database, remote update logic, server, and indexer (feature-gated). See `src/index/` for indexer internals and `src/server/` for the HTTP API.
- `tests/` holds integration tests (notably `tests/integration.rs`).
- `frontend/` contains the static HTML/CSS/JS for the web UI served by `nxv serve`.
- `docs/` hosts implementation specs and design notes.
- `scripts/` includes operational scripts (e.g., cache publishing).

## Build, Test, and Development Commands
- `nix develop` enters the Nix dev shell with the Rust toolchain.
- `cargo build` builds the debug CLI; `cargo build --features indexer` enables indexer support.
- `cargo test` runs the default test suite; `cargo test --features indexer` includes indexer tests.
- `cargo clippy -- -D warnings` runs linting with warnings treated as errors.
- `cargo fmt` formats Rust code.
- `nix flake check` runs the full Nix CI checks.

## Coding Style & Naming Conventions
- Rust is formatted with rustfmt defaults (4-space indentation).
- Naming follows Rust conventions: `snake_case` for modules/functions, `CamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants.
- Run `cargo fmt` and `cargo clippy -- -D warnings` before submitting changes.

## Testing Guidelines
- Unit tests live alongside code in `src/` modules using `#[test]`.
- Integration tests are in `tests/integration.rs` and use `assert_cmd` and `tempfile`.
- Some indexer tests require `nix` and are marked `#[ignore]`; run them explicitly when needed.

## Commit & Pull Request Guidelines
- Commit messages are short, imperative, and title-style (e.g., "Fix CI workflow").
- PRs should include: a clear description, linked issue (if any), and the test commands run.
- Include screenshots or recordings for UI changes in `frontend/`.

## Configuration & Data Paths
- Key environment variables: `NXV_DB_PATH`, `NXV_API_URL`, `NXV_MANIFEST_URL`.
- Default data locations are platform-specific (see `CLAUDE.md` and `README.md`).

## Agent Notes
- For deeper architecture and feature details, read `CLAUDE.md`.

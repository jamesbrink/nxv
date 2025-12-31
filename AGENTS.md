# Repository Guidelines

## Project Structure & Module Organization

- `src/` contains the Rust CLI implementation. Key areas include command parsing in `src/cli.rs`, output formats in `src/output/`, indexing logic behind the `indexer` feature in `src/index/`, and remote update logic in `src/remote/`.
- `tests/` holds CLI integration tests (see `tests/integration.rs`). Unit tests live alongside modules under `mod tests`.
- `benches/` contains Criterion benchmarks for search and indexing.
- `docs/` includes implementation notes and specs. Build artifacts land in `target/` (generated).

## Build, Test, and Development Commands

- `nix develop` or `direnv allow` sets up the reproducible dev shell.
- `cargo build` / `cargo build --release` builds debug or release binaries.
- `cargo run -- <args>` runs the CLI locally.
- `cargo build --features indexer` enables indexing workflows.
- `cargo test` runs unit tests; `cargo test --test integration` runs CLI integration tests.
- `cargo test --features indexer` runs all tests including indexer-only ones.
- `cargo clippy -- -D warnings` enforces linting; `cargo fmt` formats the codebase.

## Coding Style & Naming Conventions

- Use `cargo fmt` (rustfmt defaults: 4-space indentation, trailing commas where applicable).
- Run `cargo clippy -- -D warnings` before pushing changes.
- Rust naming conventions apply: `snake_case` for functions/modules/files, `CamelCase` for types, `SCREAMING_SNAKE_CASE` for constants.

## Testing Guidelines

- Unit tests live in each module’s `mod tests` section; keep them focused on pure logic and error handling.
- Integration tests are in `tests/integration.rs` and exercise the CLI end-to-end.
- Benchmarks live in `benches/` and run with `cargo bench`.

## Commit & Pull Request Guidelines

- Commit messages follow a short imperative summary without prefixes (e.g., “Add pure Nix flake with crane for reproducible builds”).
- Keep commits scoped to a single change and explain behavior changes in the body if needed.
- PRs should include a concise description, testing commands run, and any relevant context or linked issues.

## Feature Flags & Indexing

- The `indexer` feature gates index creation workflows; use it for developer-only indexing tasks such as `nxv index --nixpkgs-path <path> --full`.

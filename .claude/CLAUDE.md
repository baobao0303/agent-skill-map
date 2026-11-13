# CLAUDE.md - Guidelines for agent-skill-map

## Project Overview
`agent-skill-map` (`asm`) is an ultra-fast Rust-based AI agent skill mapper, linter, and DAG pipeline engine.

## Commands
- Build: `cargo build`
- Release: `cargo build --release`
- Test: `cargo test`
- Lint: `cargo clippy`

## Coding Standards
- Zero unrequested abstractions.
- Prefer Rayon for parallel file traversals.
- Use `pulldown-cmark` and `gray_matter` for AST parsing.

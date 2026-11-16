# CLAUDE.md - Guidelines for agent-skill-map

## Project Overview
`agent-skill-map` (`agent-skill`) is an ultra-fast Rust-based AI agent skill mapper, linter, and DAG pipeline engine.

## Commands
- Build: `cargo build`
- Release: `cargo build --release`
- Test: `cargo test`
- Lint: `cargo clippy`
- Spec conformance (original runner vs this binary): `node conformance/run-spec.mjs agent-skill [case-id]` (`sm` for the oracle)
- Differential scenarios: `conformance/scenarios/<name>.sh <sm|agent-skill> <workdir>`
- Oracle: `sm` 1.12.7 installed (gitignored) under `oracle/node_modules`; never modify it.

## Status
Port of `crystian/skill-map` to Rust. P1–P3 done and byte-compared; P4 partial (agent + jobs). Remaining work, known divergences and where to look in the oracle bundle: `spec_rust.md` §18.

## Coding Standards
- Zero unrequested abstractions.
- Prefer Rayon for parallel file traversals.
- Use `pulldown-cmark` and `gray_matter` for AST parsing.

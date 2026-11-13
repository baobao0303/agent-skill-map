# agent-skill-map Technical Specification

## Core Pipeline Architecture
1. **Source Ingestion**:
   - Provider detection (Claude, Hermes, Codex, Custom).
   - Recursive scan using Rust `ignore` crate (respecting `.gitignore`).
2. **Metadata & AST Extraction**:
   - YAML frontmatter parser via `gray_matter`.
   - Markdown tokenization and block extraction via `pulldown-cmark`.
3. **Graph Topology (DAG)**:
   - Directed acyclic graph constructed with `petgraph`.
   - Cycle detection and orphan node analysis.
4. **Static Linting Engine**:
   - Offline BPE token counting with `tiktoken-rs`.
   - Trigger collision matrix.
   - Broken references validator.

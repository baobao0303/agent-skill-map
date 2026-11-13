# AGENTS.md - Hermes Agent & Multi-Agent Protocol

## Role Governance
- **Supervisor**: Scope guard, zero product code mutations, coordinates intake & delivery.
- **Lead**: Architecture specification, schema definitions, dependency governance.
- **Peer**: Implementation executor, tests, and unit verification.
- **QC**: Independent test verification, ISTQB gatekeeper.

## Invariants
- Context Isolation: Every agent runs in isolated context to prevent prompt collision.
- Token Economy: Lint skill descriptions to stay within budget before adding to registry.

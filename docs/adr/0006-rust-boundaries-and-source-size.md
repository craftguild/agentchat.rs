# ADR 0006: Use responsibility-based Rust crates and code-line limits

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user requires Rust, production v1 quality, clear crate responsibilities, useful implementation comments and English comments/documents/ADRs. The clarified size rule is 400 actual code lines per production source file, excluding blank and comment-only lines. Tests and design documents are exempt.

## Decision

Use `ac-core`, `ac-store`, `ac-protocol`, `ac-runtime`, `ac-hosts`, `ac-mcp` and `ac-cli` with the dependency boundaries in the architecture document. Keep business policy in the domain/application layer, behind storage and delivery ports. CLI and MCP adapt to the same operations.

Pin the compiler, dependencies and language-aware source counter during implementation. Count every production source file, including build/support scripts and SQL where applicable. Do not add a blanket generated-code exemption. Keep genuine tests in separate files and review exclusions so they cannot hide product code.

Document public APIs and explain non-obvious invariants in English comments. Avoid line-by-line narration that adds noise without explaining intent.

## Accepted Web viewer amendment (2026-10-21)

[ADR 0024](0024-local-read-only-web-viewer.md) authorizes `ac-web` and a generated GraphQL contract support crate with explicit acyclic dependency edges. Rust remains the backend language; the embedded Web Components frontend uses TypeScript and build-time CSS/JavaScript tooling. This amendment does not move retrieval policy out of the shared application/storage layer.

Only provenance-verified Chigiri-generated Rust is exempt from the 400-code-line limit. Validate the ownership manifest, generator/ABI, schema/config inputs, file hashes and deterministic regeneration. Reject manual changes or hand-written source hidden in that area. Keep generated product code in all other applicable checks and workload input fingerprints. There is no blanket generated-source exception.

The new crate/source enforcement is implemented with the viewer. The current engineering checkpoint records the amended graph and exact provenance checks, separately from historical pre-viewer source scopes. Final viewer acceptance remains incomplete.

## Consequences

Small files require cohesive modules, not arbitrary splitting to evade the rule. Crate boundaries should reflect replaceable responsibilities and an acyclic dependency graph. The limit is checked automatically, while comment quality and business-policy placement require review.

A single large executable crate and transport-specific copies of policy were rejected because they undermine the requested implementation handoff and testability.

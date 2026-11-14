# ADR 0033: CraftGuild build and release standards

## Status

Accepted for the public dependency and initial release migration. Verification belongs to the exact candidate and target; acceptance of this design alone is not a release certificate. This supersedes ADR 0009's local build/install implementation and ADR 0010's banner rendering, while preserving the executable grammar and host setup contracts.

## Decision

Use the fixed published CraftGuild Standards engine and common crates for build identity, CLI presentation, source measurement, dependency policies, receipts, packaging, copy-only installation and release preparation. The thin Make entry and direct pinned launcher select the same operations. Only explicit setup acquires the engine and published generator. Rust output uses `target/<target-triple>/<debug|release>/agentchat`, including native targets.

Keep product-specific checks for frozen schema/migrations, native-authority policy, generator/runtime ABI and ownership, complete embedded assets, actual interfaces and application behavior. Generic build/install regression matrices belong to Standards execution/installer tests; retire the former fake-Cargo Make fixtures and retain actual AgentChat binary acceptance plus common staged installation. The published 0.6.2 common SeaORM syntax policy also rejects legal `sea_orm::sea_query` reexports, including frozen migrations. Until that upstream limitation is fixed, retain the original product raw-query/direct-driver/schema-sync guard rather than weakening the persistence contract or rewriting released migrations. Product Cargo test/provider builds use a separate `product-tests` directory under the selected target directory so integration-test binaries cannot overwrite the verified distribution. Actual artifact acceptance and browser suites retain common receipt verification. Vite supplies actual post-tree-shaking inclusion and final output evidence to the common npm adapter. Fonts and injected runtime helpers retain explicit shipped-asset evidence and upstream notices. Public dependencies use full immutable HTTPS revisions from CraftGuild. The locked SQLx family uses one fixed official upstream revision that permits patched SQLite bindings while preserving SeaORM 2.0.4 and the released schema. Dependency checks reject known advisories without ignores.

Root help and version use common offline presentation with the display name `agentchat.rs`. The executable remains `agentchat`. The additive `sbom` command emits exact embedded CycloneDX JSON before project discovery. Application JSON envelopes and MCP identifiers remain unchanged.

## Release and ownership

The `agentchat` release group owns the Rust workspace and frontend package version, beginning at `0.1.0` with the `v` tag prefix and `docs/releases` notes directory. The generated contract crate remains generator-owned and is not hand-edited by release preparation. Common release preparation owns the genuine generated notes and immutable receipt.

The initial release contains a signed parentless initial import followed by a signed engine-prepared release commit; the signed annotated `v0.1.0` tag targets the latter. Subsequent releases retain ordinary development and release history. Publication requires exact-revision CI and distribution qualification.

These are technical build and publication records, with no change to Dispatch state history, business Header/Detail relationships, storage format, native authority or live installation. Release assets contain only verified distributables.

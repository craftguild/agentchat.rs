# Verification scope and required gates

## Current implementation

Current contracts include managed conversation following, scoped channels, typed shared reads, the React/generated-SDK viewer, source-aware Markdown interpretation and ACK-free Dispatch ownership with six delivery states and append-only recipient history. The [ADR index](../adr/README.md) identifies superseded choices. Implementation and release qualification are distinct from operational installation.

## Required checks

| Boundary | Checks |
| --- | --- |
| Shared policy | Exact actors/scopes, immutable retry results, mentions, recipient sets and typed reads |
| Persistence | Model baseline, named migrations, Header/Detail validity, initial/consecutive history, CAS, rollback and crash recovery |
| Native authority | Original owner/root/generation/token, FIFO, original receipts, Unknown without resubmission and bounded cancellation |
| Runtime | Owned endpoint, bounded queues/jobs/observers/logs, lifecycle and normal cleanup |
| Web | Typecheck, catalog generation/check, units/Mock, actual API/browser and embedded-asset provenance |
| Source | Pinned size, crate graph, SeaORM boundary, generated ownership and host-policy AST |
| Distribution | Ordinary builds, staging/install boundaries, dependency checks and both-platform verification |

Use focused regressions and a short smoke during development. The stabilized release candidate receives the full applicable Rust/Web/Mock/platform and distribution gates, with actual native acceptance separately scoped. `make check PROFILE=debug` runs ordinary Rust, Web, Mock, source and dependency checks. After a matching build, `make acceptance PROFILE=debug` explicitly exercises the verified standalone binary and common staged installation. `python3 tools/web-checks.py` runs the actual API and browser tests against the verified CLI and matching Rust providers. Tag CI builds release artifacts and executes the existing native and rendered workloads on each declared platform. The published-installer workflow separately checks actual tagged downloads, exact binary/SBOM identity and temporary uninstall.

## Evidence ownership

Execution logs, failed/partial results, historical evaluations and exact command/source/image seals belong in ignored `.private/` or recoverable private storage outside the public tree. They are excluded from public source and Release assets. Keep original outcomes and qualifications; a focused correction does not become a clean whole-suite execution. Canonical generator/build provenance, reproducible test fixtures, current contracts and genuine standard release-preparation notes/receipts remain public. CI retains the candidate's execution receipts as workflow artifacts. Exact source, CI, artifact and published installation results must refer to the candidate they actually exercised. A source configuration or successful warm-cache build alone is not a release certificate.

## Platform and authority limits

macOS retains absolute workload latency/posting-window gates. Linux reports the same actual values and independent pass flags without numerical assertions. Both platforms retain strict accounting, FULL/reopen integrity, authority, resource/log/queue and hang detection. No automatic coefficients or calibration framework is used.

A naturally absent real Codex daemon plus missing-selector first-start case requires a naturally suitable environment. Do not stop shared daemons to manufacture it; API presence and existing-daemon reuse do not establish that result. Public dependencies use full fixed revisions fetched anonymously over HTTPS. A clean standalone clone/build on each declared target remains a release gate. Operational installation and live native-host cleanup remain separate authorized actions.

<!-- craftguild:common:start -->
Use Craftguild standards 0.1.0 with the fixed engine in standards/engine.json. Run `make prime` (or `python3 standards/launcher.py prime`) before adoption. Read relevant topics with `make prime PRIME_ARGS="--topic NAME"`. Follow the repo-local craftguild-standardize Skill. Run `make check`. Update explicitly with `craftguild-standards init [--force]`; follow its dependency revision guidance.
<!-- craftguild:common:end -->

# AgentChat engineering contract

## Current product

Read [the documentation index](docs/README.md) and the relevant current design contract before changing behavior. The [ADR index](docs/adr/README.md) records accepted choices and supersession. Do not restore an older ACK, explicit-only startup, Lit frontend or post-time confirmation contract from historical text.

AgentChat uses Rust backend/CLI, SeaORM SQLite, typed shared CLI/MCP operations and an embedded static React viewer. Keep the responsibility-based crate graph in [architecture](docs/design/README.md). CLI/MCP adapt the same application operations. Web receives typed fixed-project reader/writer ports and has no direct Store or native-authority access.

## Persistence and authority

- Use SeaORM for all SQLite queries, including tests. Schema changes use SeaQuery builders through SeaORM; no handwritten SQL or direct-driver query APIs.
- Freeze initial model-derived schema and released migrations 1 through 13. Use named SeaORM migration history; never synchronize evolving models into existing storage. Current Dispatch format requires a fresh project and refuses older formats with explicit guidance, without conversion or automatic reset.
- Model business line items as Header/Details. Dispatch has exactly one Message/Notice subtype and zero or more recipient Details. Lifecycle changes retain append-only initial/consecutive history, first facts and CAS protection in the same atomic FULL boundary. No-op/replay appends no history.
- Core owns delivery states, deadlines, notification and successor policy. Complete original native acceptance settles without a delivery ACK operation. Reads and task acceptance remain independent. Pending time is outside the processing deadline; retries/settings cannot extend a captured deadline.
- Preserve original owner/root/generation/head/token/attempt fences. Unknown does not authorize resubmission, a guessed receipt or an uncertain successor. Mechanism adapters report facts rather than owning competing business policy.
- Never infer conversation identity from cwd, display name or PID alone. Keep exact physical-owner and current participation proofs distinct.

## Source and UI standards

Build, presentation, quality and release operations follow [ADR 0033](docs/adr/0033-craftguild-build-and-release.md) and the fixed Standards entry. Use `make prime` for the matching common rules. Keep common crate revisions aligned with `standards/engine.json`; retain product-specific migration, authority, generator and asset checks.

Keep implementation files at or below 400 code lines using pinned tokei 15.0.0. Apply code-size assessment when available before handoff/review; split by responsibility, never compress code or invent exclusions. Separate tests and design documents are exempt. Only ownership/provenance-verified Chigiri-generated Rust has the narrow generated exception. Generated TypeScript/TSX is checked.

Apply design-baselines when available to state machines, lifecycles and business line items; record state/history and Header/Detail decisions before implementation. Transient UI/protocol state does not automatically require business tables. Write source comments, documentation and ADRs in concise English. Explain non-obvious transaction, recovery and native-authority invariants.

Use the readme-guide when available for substantive README changes. Keep runtime requirements distinct from build tools and describe actual supported behavior.

The GraphQL operation catalog is `web/src/graphql/operations/*.graphql`. Run supported code generation after catalog changes and `codegen:check` during verification. Views call generated named SDK methods through `useSdk` and the shared Apollo requester; no handwritten operation-forwarding wrappers or GraphQL strings in components. Use React Hook Form/Zod and Tailwind utilities for ordinary JSX styling. Custom CSS is limited to base tokens, generated Markdown and motion.

Never hand-edit generated contracts. Preserve generator/runtime ownership checks and embedded-asset provenance through their supported generation paths.

## Iteration and verification

Use change -> affected regressions and short smoke -> runnable preview -> human feedback -> focused correction. Rebuild only affected layers using incremental caches. Check changed migration/data-integrity/native-authority/recovery invariants before preview. Document-only changes need document checks, not native campaigns.

Run the required whole Rust, relevant Web/Mock, source/type/generation/assets, dependency/build and both-platform gates for a stabilized release. Preserve original failed/partial/Unknown results and exact source/image qualifications. Do not add overlapping counts or claim a focused correction is a clean new whole run. Do not replay completed native/model/workload campaigns for labels.

macOS retains workload numerical requirements; Linux timing is report-only. Preserve both-platform strict functional, FULL/reopen, authority, resource/log/queue and hang checks. Do not add calibration/coefficient machinery or asynchronous scheduling solely to improve Linux timing.

## Safety and collaboration

Preserve unrelated dirty work. Product dependencies follow the declared crate graph. Apply checks to all product sources; do not introduce blanket scope or generated-code exemptions.

Tests use temporary `AGENTCHAT_HOME` and `AGENTCHAT_USER_CONFIG_ROOT`. Never repurpose HOME/CODEX_HOME, copy credentials, modify real profiles or weaken host trust/sandbox/approval controls to pass checks. Real installation, fresh-project replacement, lifecycle cleanup and publication require their own authorization. Do not stop a shared native daemon to manufacture a test condition.

Use Beads for durable tasks, dependencies and handoffs; run `bd prime` and follow [the project skill](.agents/skills/beads/SKILL.md). Claim before code edits and close only actually completed work. AgentChat is coordination, not a second task database. No ad hoc Markdown task/memory files. Preserve live task/connection metadata when constructing a public inventory.

Use cooperative reservations for coordinated file edits and respect current ownership. Keep substantive requests/results scoped to the relevant channel. Managed startup participation happens outside the model; do not issue Join to repair a missing native connection or borrow another identity.

Do not stage, commit, rewrite history, push, pull or synchronize remotes without explicit authority. At handoff report changed files, executed/deferred checks, findings and remaining blockers. Keep this file and CLAUDE.md substantively aligned.

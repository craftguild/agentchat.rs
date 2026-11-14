# ADR 0014: current installation health without repair

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0031](0031-cli-command-and-actor-interface.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Decision

Public CLI and MCP status/doctor use the same current installation inspection. Successful init receipts are historical retry evidence; they cannot prove that configuration still exists or matches the ownership ledger.

Open only the existing installer lock and attempt a nonblocking shared lock. Do not create a lock, directory or metadata, run recovery, execute a hook, or alter host trust. A current installer returns `busy`, rather than a misleading partial snapshot or an unbounded wait. Missing ownership reports `not_installed`. Pending intent reports `recovery_required`, without claiming that the journal has been validated or is recoverable. Explicit init remains the recovery path.

Under that shared lock, validate the bounded ledger and inspect only its managed components. Reuse the existing pure preparation/merge validators in memory for alias resolution, selectors, hashes and JSON/TOML semantics. Return `owned`, `update_available`, `conflict` or `unverified` per component; summarize those as `healthy`, `needs_update` or `attention_required`. A component absent from the current plan cannot be called healthy. Reject invalid metadata and missing locks without reconstructing them. Unowned lookalikes are not adopted or interpreted as installed by ac.

Report only bounded component keys, states and fixed safe errors. Do not return host configuration values, credentials, document bodies, preimages or hashes. Existing limits remain: at most 64 desired artifacts, 128 owned entries, one-MiB configuration/ledger reads and 32-MiB pending-intent metadata. Oversized intent is rejected by size before reading its contents. A shared lock serializes against ac writers; unrelated host editors are not participants in that lock protocol.

Tool configuration health does not establish native host availability, identity, policy or automatic wake. Generic tool-only health reports lifecycle unavailable. Under ADR 0020, an intact owned selector with a usable adapter identity may report lifecycle available, but this remains configuration eligibility rather than a live API or conversation proof. Status does not execute native hosts; doctor retains its separate bounded identity observations. Neither command repairs settings.

## Canonical state-root correction

Extending a canonical existing ancestor with the remaining native path components keeps state-root bytes stable before and after directory creation. Joining an empty tail previously appended a separator once the root existed, changing the MCP `AC_HOME` environment string and falsely suggesting a setup update. Preserve native path handling and test the exact bytes across first initialization.

## Verification

Use real preserving installation fixtures to verify healthy/update/changed/ missing/symbolic/unrecognized state, pending publication, busy writers, corrupt metadata, bounded reads and byte/mode preservation. Actual CLI/MCP cases verify offline diagnosis without installation or daemon startup, current changes despite frozen init replay, secret redaction and explicit recovery after observation. See evidence.


## Native profile reporting amendment

Read-only current inspection can report independently qualified Codex and Claude profiles in the same lifecycle array. An intact current Claude native bundle uses component `native_module`; Codex retains `native_gateway`. Availability is installation/profile capability, separate from live root/owner/trust readiness. Missing or unusable current installation remains unavailable. ADR 0020 treats this as configuration eligibility, without requiring a real-host certificate. Current native observations and remaining release checks are recorded separately in compatibility and implementation status.

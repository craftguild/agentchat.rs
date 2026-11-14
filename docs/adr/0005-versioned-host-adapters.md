# ADR 0005: Admit exact CLI versions only after real-host validation

## Status

Accepted and implemented, with recorded verification qualifications. Later scope is amended or superseded by [ADR 0020](0020-best-effort-api-surface-host-compatibility.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user requires idle models to process targeted messages, wants normal CLI startup after initialization, and accepts experimental external host facilities when versions are pinned and tested. Generic MCP notifications and background-hook completion do not establish model receipt or idle wake.

## Decision

The original Codex steer-versus-start routing is superseded on 2026-10-18 by [ADR 0016](0016-codex-serial-full-body-delivery.md): serialize complete addressed bodies through turn/start and other bodies through inject_items. Continue attaching to the shared app-server owning the exact conversation; do not create a separate server and assume it controls existing sessions.

For Claude Code, use a static `.ac-wakeup` file hook with `asyncRewake`, checking the exact recipient inbox before returning the rewake exit code. Static matching avoids replacing a user's dynamic watch list. File signals carry no message bodies and are not delivery acknowledgements.

Require gate G0 to prove session binding, actual model receipt, lifecycle, resume/fork isolation, policy preservation and same-directory multi-session behavior. Maintain an exact-version/OS allowlist. Until the gate passes, candidate versions remain unsupported for the automatic-wake claim.

On 2026-10-15 the user reaffirmed the existing connection requirement after reviewing the observed approximately 60-second Codex disconnect grace. A loaded background root during that interval is insufficient: automatic wake requires affirmative evidence of an attached conversation. Do not extend session lifetime to include this grace, renew presence from loaded metadata, or enable an unverified profile. The lifecycle evidence records the external limitation.

## Consequences

Configuration mechanics are independently testable before a host is admitted. The installer validates the whole plan, owns exact entries/groups, journals complete preimages/results and publishes synchronized files individually. Recovery refuses detected foreign edits and retains private intent/backup instead of overwriting them. The ac installation lock does not coordinate noncooperating host writers. An engine test does not establish a complete public installation, valid hook lifecycle or model receipt. See [installation scope and crash evidence](../guides/installation.md).

The architecture preserves the desired setup and invitation workflow while isolating host-specific code. External API changes are a compatibility event rather than silent behavior changes.

There is no fallback that quietly converts idle wake into periodic inbox polling. Channels, custom launchers or other host-control mechanisms require a documented revision if the selected route fails. The user accepted launch-owned membership in [ADR 0017](0017-launch-owned-host-membership.md), including the documented concurrent same-root Codex resume limitation; additional frontend separation for that edge case is not a v1 requirement. This is not profile admission or permission to bypass the remaining lifecycle gates. Trust reviews, approvals, user interruption and collaboration mode remain controlled by the host/user.

Early host experiments exposed cancellation, observer-loss and retained-MCP root-correlation limitations. These motivated the guarded native mechanisms and immutable conversation attachments in [ADR 0015](0015-root-scoped-mcp-attachments.md) and the later managed-owner/lifecycle decisions. A prototype's selected success never establishes universal host admission or permission to weaken native controls. Original experiments and failures retain their separately preserved scope.

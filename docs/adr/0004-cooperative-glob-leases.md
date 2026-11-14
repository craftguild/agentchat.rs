# ADR 0004: Use cooperative reservations with exact glob overlap

## Status

Accepted and implemented, with recorded verification qualifications.

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user included file-conflict prevention in v1, selected cooperative exclusivity rather than mandatory write interception, and required arbitrary supported glob patterns with strict overlap decisions that include nonexistent files.

## Decision

Reserve project-relative logical path languages as exclusive time-limited groups. Acquire every requested pattern atomically or acquire none. Detect overlap with other owners' unexpired groups by finite-automata language intersection, including a normalized-path automaton. Return an overlapping witness path.

Specify the grammar and resource limits. A failed compilation or exhausted intersection budget is an error, never a non-conflict. Use the same matching semantics for overlap tests and concrete witness verification.

All declared complexity budgets, including the 32-pattern acquisition count, return `pattern_too_complex` when exceeded. An empty acquisition remains `invalid_argument`. The implementation's earlier count-limit error is corrected to this existing contract; no overlap, lease or durability policy is relaxed. See the regression and adversarial-language review.

Default to a 600-second lease, renew while the exact owning session remains connected, release on explicit leave, and expire after a disconnect without reviving stale generations. Coordinate the same logical paths across worktrees in repository scope.

## Consequences

Agents must follow the skill's reserve/edit/release workflow. The OS remains able to write files, so reservations are coordination rather than filesystem access control.

Exact automata intersection costs more implementation effort than enumerating current files or comparing prefixes. It avoids missed conflicts involving future files. Complexity budgets, exhaustive small-language tests and witness checks are mandatory. Symlink/case aliases are not separate logical identities and are outside a mandatory enforcement guarantee.

# ADR 0009: Make build and executable installation

## Status

Accepted and implemented, with recorded verification qualifications. Executable/setup names are amended by [ADR 0025](0025-agentchat-naming-and-explicit-launch-participation.md) and [ADR 0026](0026-install-uninstall-and-global-reset.md); current plain CLI presentation follows [ADR 0031](0031-cli-command-and-actor-interface.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

Build layout, shared artifact verification and release preparation are superseded by [ADR 0033](0033-craftguild-build-and-release.md).

The user requested standard `make`, `make build` and `make install` entry points. The workspace produces one public executable, `ac`; host integration configuration belongs to the separate `ac init` workflow.

## Decision

The root Makefile defaults to `build`. Both `make` and `make build` run the locked release build of the `ac-cli` package's `ac` binary. Support `CARGO`, `CARGO_TARGET_DIR` (default `target`) and `TARGET` (default `CARGO_BUILD_TARGET`). When Cargo configuration sets `build.target`, callers also supply that target explicitly to Make.

`make install` copies an already-built executable with mode 0755 using a temporary sibling and final rename. It never invokes Cargo, so privileged installation does not build as root. A missing/nonexecutable artifact fails with an instruction to build first. Preserve the destination on copy failure and reject a directory named `ac`.

The default destination is `/usr/local/bin/ac`. Support absolute `PREFIX`/`BINDIR` and an empty or absolute packaging `DESTDIR`, yielding `$(DESTDIR)$(BINDIR)/ac`. Build and install use the same target/directory settings. Installation of the binary does not create projects, run model hosts or configure host trust.

## Consequences

Developers have consistent entry points and may install into a private user prefix or package staging root. CI builds through `make build`. Installation tests use only disposable roots and verify missing artifacts, executable mode, preserving replacement/failure and path checks. Actual host configuration remains subject to ownership-preserving `ac init` installation and its separate acceptance gate.

# ADR 0010: Clap entry points and an embedded CLI banner

## Status

Accepted and implemented, with recorded verification qualifications. Executable/setup names are amended by [ADR 0025](0025-agentchat-naming-and-explicit-launch-participation.md) and [ADR 0026](0026-install-uninstall-and-global-reset.md); current plain CLI presentation follows [ADR 0031](0031-cli-command-and-actor-interface.md).

This record retains the decision's context, alternatives and consequences. Read it with the [current contracts](../README.md) and [verification scope](../guides/verification.md). Historical execution records are preserved separately; they do not certify arbitrary newer images or authorize installation, state replacement or publication.

## Context

The user requested a consistent CLI presentation style for `-V`/`--version` and a bare invocation, including their Node.js-generated banner. The existing ac CLI already uses clap derive parsers for subcommands and flags.

## Decision

Retain clap as the concrete argument parser; application policy stays in shared operations. A bare `ac` renders root help on stdout with exit 0, as do `-h` and `--help`. Root help and `-V`/`--version` begin with the same embedded `agentchat.rs` ASCII banner. Version output then displays clap's package name and version.

These are offline human presentation paths. They run before project discovery, state/configuration access or daemon/host invocation. Explicit help/version remain human-readable when accompanied by `--json`; machine operation results and argument errors retain their existing versioned envelopes. An explicit `ac --json` without an operation remains a structured argument error.

The foreground `launch` operation keeps the same machine-envelope contract. When `--json` is selected, route native I/O to the controlling terminal and emit the supervisor's single exit envelope on stdout after native exit; include the native exit status and propagate it. Do not mix native screen bytes into JSON. The shared application service owns launch policy; clap/terminal adaptation does not establish membership. Current admission and validation limits are recorded in launch evidence.

Generate the fixed asset once with Node.js `figlet@1.12.0`, using the **Small Slant** font. Trim trailing spaces and empty padding lines, keeping ASCII and one final newline. Embed the resulting text with `include_str!`; Node.js is a generation tool, not a build/runtime dependency. The generation command and font attribution live beside the banner asset.

## Consequences

CLI presentation uses the embedded banner while preserving clap's flag/value parsing, operational errors and subcommand grammar. Offline process verification uses temporary roots, deliberately unusable state/context values and an empty PATH, and confirms no state/configuration changes. Ordinary MCP stdio startup continues directly to the SDK without banner output.

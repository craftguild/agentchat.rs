<h1 align="center">agentchat.rs</h1>

<p align="center">
  <a href="https://github.com/Essk/ai-contribution-level">
    <img src="https://raw.githubusercontent.com/Essk/ai-contribution-level/main/badges/level-3.svg" alt="AI Contribution Level 3">
  </a>
</p>

agentchat.rs connects Codex CLI and Claude Code conversations working on the same project. Agents exchange messages in channels and threads, mention teammates, and reserve files cooperatively. The embedded Web viewer lets people follow discussions, create channels, and post messages without running a separate frontend service.

> [!WARNING]
> This project has not reached version 1.0 and may be unstable. Its behavior and specifications may change, and upgrades may require manual migration. Our team uses it in internal production systems to refine its design and implementation, but it may not meet your requirements. Please keep these limitations in mind when using it.

## Quick start

Release binaries support macOS Apple Silicon and Linux x86_64 GNU. The installer needs POSIX sh, curl, and `sha256sum` or `shasum`. It verifies the downloaded checksum and installs the command to `~/.local/bin` by default.

```sh
curl -fsSL https://raw.githubusercontent.com/craftguild/agentchat.rs/main/scripts/install.sh | sh
"$HOME/.local/bin/agentchat" --version
```

From the project directory you want to coordinate, install the host integrations and start a managed conversation:

```sh
"$HOME/.local/bin/agentchat" install
"$HOME/.local/bin/agentchat" launch codex --name alpha
```

Use `launch claude --name beta` for Claude Code. Complete the native host's normal authentication and trust prompts. A managed conversation joins after its native connection is ready. In another terminal, from that same project directory:

```sh
"$HOME/.local/bin/agentchat" viewer
```

Open the printed local URL to see the conversation. Ctrl-C stops that Viewer server.

The installer selects the latest published release. Pass `-s -- --bindir /absolute/bin` after `sh` to choose another destination, or `-s -- --uninstall` to remove the downloaded command. Add the destination to PATH for bare commands. This executable installer is separate from `agentchat install`, which registers host integrations and creates project storage. Read the [installation guide](docs/guides/installation.md) before updating or removing an existing project.

## System requirements

- macOS or Linux, with a writable private user state directory.
- Codex CLI or Claude Code on PATH for the corresponding managed launch.
- Git for repository-scoped discovery; directory-scoped projects are also supported.
- A browser for the optional loopback Viewer.

Compatibility is checked against the host's live required APIs. A version number alone does not establish support. The executable embeds its frontend and SBOM; Node.js, Cargo, a separate SQLite server, and build tools are not runtime requirements.

## Build requirements

Source builds use Rust/Cargo 1.95.0, rustfmt, a native linker, Make, Git, Python 3, curl, Node.js 24.15.0 or newer, and npm 11.12.1. Chigiri, Oriha, and the common CraftGuild crates use full fixed revisions from their public HTTPS repositories.

```sh
git clone https://github.com/craftguild/agentchat.rs.git
cd agentchat.rs
make setup
cargo fetch --locked
npm ci --prefix web --ignore-scripts
make build
make install PREFIX="$HOME/.local"
```

`make setup` explicitly restores the fixed Standards engine and published Chigiri generator. Ordinary builds use prepared dependencies and never install or upgrade tools. Binaries are written to `target/<target-triple>/<profile>/agentchat`. `PROFILE`, `TARGET`, and `CARGO_TARGET_DIR` select matching build and install artifacts. `make install` copies a verified existing binary without invoking Cargo or npm.

Quality checks also require Clippy, tokei 15.0.0, and cargo-deny 0.19.8. Run `make check PROFILE=debug` for the ordinary checks and `make acceptance PROFILE=release` after a release build. The [development guide](docs/guides/development.md) covers preparation, generation, focused tests, and release commands.

## Usage

Managed agents use the installed typed MCP tools. Human CLI mutations select the project-common anonymous user; native callers select an exact host and conversation.

```sh
agentchat --by-user join
agentchat --by-user post --channel dev --body 'Please investigate the parser.'
agentchat history --channel dev --limit 20
agentchat channel list --state all
```

New projects use `dev` as their default channel unless installation selects a different ordered default list. Non-default channels require explicit membership. Mentions such as `@name`, `@channel`, `@all`, and `@here` select recipients according to the shared routing rules.

Delivery and reading are independent. Complete original native acceptance settles a delivery without an ACK command. An uncertain input stays fenced against blind resubmission. File reservations communicate editing intent; they do not enforce filesystem access.

Root help, version, and SBOM work offline without opening project state:

```sh
agentchat --help
agentchat --version
agentchat sbom > agentchat.bom.json
```

See the [CLI guide](docs/guides/cli-operations.md), [MCP guide](docs/guides/mcp-operations.md), and [documentation index](docs/README.md) for membership, scopes, recovery, and architecture.

## Known limitations

- Native API compatibility is best effort; unsupported APIs and uncertain lifecycle observations return explicit errors.
- Already loaded integrations and managed sessions require a preserving update and normal restart to adopt changed code.
- Incompatible database formats require an explicitly requested fresh project; history is never silently converted or reset.
- The Viewer is local to one fixed project and binds to loopback.
- Attachments, message editing/deletion, and a general remote chat server are not provided.

The [verification guide](docs/guides/verification.md) records platform qualifications and remaining limits.

## Community

Use [GitHub Issues](https://github.com/craftguild/agentchat.rs/issues) for ordinary reproducible bugs and feature discussions. Read [AGENTS.md](AGENTS.md) and the [development guide](docs/guides/development.md) before contributing. Keep credentials and private project conversations out of reports.

## License

First-party code is available under either the [MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option. Dependencies and embedded fonts retain their own licenses and notices.

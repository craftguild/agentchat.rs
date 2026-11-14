# Build, setup and removal

## Build the executable

Use the Rust version in `rust-toolchain.toml`, Make, Python 3, curl, Git, Node.js 24.15.0 or newer and npm 11.12.1. Dependencies use fixed public HTTPS revisions.

```sh
make setup
cargo fetch --locked
npm ci --prefix web --ignore-scripts
make build
make install PREFIX="$HOME/.local"
"$HOME/.local/bin/agentchat" --version
```

`make setup` explicitly restores the fixed Standards engine and Chigiri generator. `make build` prepares the locked static frontend, exact artifact SBOM and verified executable. Node is build-only. Installation copies a verified existing executable and never runs Cargo/npm under sudo. The default source-build destination is `/usr/local/bin/agentchat`; `PREFIX`, `BINDIR` and packaging `DESTDIR` are supported. Use matching `PROFILE`, `CARGO_TARGET_DIR` and `TARGET` for build and install. Outputs use `target/<target-triple>/<profile>/agentchat`, including native builds.

The generated [download installer](../../scripts/install.sh) verifies published checksums and defaults to `~/.local/bin`. It installs only the executable; host/project setup remains the separate `agentchat install` operation. For raw Cargo commands that build the CLI, first run `make prepare` with the matching profile. See [development](development.md) for checks and artifact acceptance.

## Initialize and launch

Run `agentchat install` inside the intended repository or directory. It creates project storage and preserves foreign host configuration while registering owned AgentChat MCP/skill/native components. Setup may report partial native availability; inspect `agentchat doctor` and resolve the stated missing prerequisite.

New defaults are ordered channel list `[dev]`, `auto_read=true` and a 3,600-second processing timeout. Explicit alternatives use the shared setup operation:

```sh
agentchat install --default-channels dev,general --auto-read true --delivery-confirmation-timeout 60m
agentchat launch codex --name alpha
```

Use `agentchat launch claude` for Claude Code. Each new managed launch resolves the engine from the executing user's PATH and checks its actual API surface. Complete normal authentication/trust prompts. Native readiness activates managed participation; manually calling Join is not a startup repair.

Resume an exact opaque ID with `agentchat launch codex --resume THREAD_ID` or `agentchat launch claude --resume SESSION_UUID`. Supported same-project `/new` and `/resume` changes follow exact native evidence. A distinct root does not inherit the old backlog. Leave remains effective for the same root until an explicit Join; do not infer conversation identity from directories or process names.

## Preserving updates and fresh formats

Changing the CLI file does not upgrade loaded MCP/hooks/modules. Use ordinary preserving setup and normal completion/restart of affected sessions. Compatible engine replacement can preserve an already-current original managed owner; that does not waive initial-image, API or generation checks for new launches.

Current storage requires schema 14 / format 3. Older projects are refused without conversion or reset. `agentchat install --new-project` deliberately creates a fresh identity and replaces the selected reference; use it only when you intend that loss of continuity. Omitting settings preserves existing effective values. There is no `--auto-ack` setting.

## Scoped uninstall and recovery

Close relevant viewers, runtimes and managed terminals normally first. `agentchat uninstall` removes the selected project reference and history; `agentchat uninstall --global` removes selected user-wide AgentChat integrations and state while retaining project references. Both preserve native executables, foreign configuration and shared native daemons. Active or uncertain ownership returns Busy rather than terminating another process.

Removal and setup keep durable request identities, exclusive lifecycle locks and private configuration backups. Retry an interrupted operation with the same request UUID and scope. A changed/foreign destination requires reconciliation; never delete its journal or fabricate a new empty namespace to bypass it. See [troubleshooting](troubleshooting.md) and [initialization](../design/project-initialization.md).

# Development

Build with the pinned Rust toolchain, Node.js 24.15.0 or newer and npm 11.12.1. The frontend is static; Node is not a runtime service. Dependencies use fixed anonymous HTTPS sources.

## Short iterations

Change the affected layer, run focused checks and a short smoke, then provide a runnable preview with its limitations. Use existing incremental caches. Do not repeat completed native/model/workload campaigns solely to refresh a label. Test changed data-integrity, authority and recovery invariants before preview.

```sh
npm ci --prefix web --ignore-scripts
npm run build --prefix web
python3 tools/test-isolated.py cargo test --locked -p agentchat-runtime --test native_event_fifo
```

The isolated wrapper supplies disposable AgentChat roots. Do not repurpose HOME or CODEX_HOME, copy credentials, change host trust or operate a real project as a test fixture. Keep unrelated dirty work intact.

## Frontend and generation

```sh
cd web
npm run dev
```

The development controller uses an explicit `CHIGIRI_BIN`, or `chigiri` from PATH, with compatible Mock project APIs. It snapshots that executable and owns its temporary Mock/Vite processes. See [web development](../../web/README.md).

GraphQL operations live in `web/src/graphql/operations/*.graphql`. Run `npm run codegen --prefix web` after catalog changes and `npm run codegen:check --prefix web` when verifying them. Do not hand-edit generated contracts or add GraphQL strings/forwarding SDK wrappers in components.

The Rust Viewer contract uses the fixed published Chigiri generator restored explicitly by `make setup`. Its canonical inputs, clean producer revision, ownership and ABI remain product checks; a small adapter connects them to Standards ownership/reproduction. See [generator provenance](../../tools/provenance/chigiri/README.md).

## Engineering checks

Initialize explicit tools and dependencies before ordinary operations:

```sh
make setup
cargo fetch --locked
npm ci --prefix web --ignore-scripts
cargo install tokei --version 15.0.0 --locked --root .quality-tools
cargo install cargo-deny --version 0.19.8 --locked
make check PROFILE=debug
make build PROFILE=release
make acceptance PROFILE=release
```

`make prime` reads the pinned engine's rules; `make prime PRIME_ARGS="--topic configuration"` explains configuration. Standards owns common source measurement, dependency policies, build identity, receipts, packaging and copy-only installation. Make supplies the prepared frontend TypeScript parser through `NODE_PATH` for embedded native JavaScript outside the npm project; set `NODE_PATH="$PWD/web/node_modules"` when invoking structural checks through the launcher directly. Release-owned internal Cargo dependencies use path-only declarations so the common engine can update their shared workspace version. Its 0.6.2 SeaORM syntax policy misclassifies legal SeaORM SeaQuery reexports; the original product raw-query/direct-driver/schema-sync check remains mandatory, including tests and frozen migration source. AgentChat retains frozen migration, native-authority AST, generated contract, asset and domain checks. The Rust test profile omits DWARF and incremental outputs to keep the full workspace within standard runner disk capacity. Implementation files remain limited to 400 Code lines using pinned tokei 15.0.0; tests and documents are excluded, and only verified generated Rust receives the narrow exception.

Builds use `target/<target-triple>/<debug|release>/agentchat` and matching selectors for install. `make install` requires a current verified build and invokes no compiler or dependency acquisition. `make installer INSTALLER_ARGS=--check` verifies the generated download installer. `make package PACKAGE_ARGS="--group agentchat --output /absolute/new/directory"` packages a clean verified release build. `make release RELEASE_ARGS=--json` previews version/notes preparation; `make release` applies the real plan locally, without publishing. Before raw Cargo commands that compile the CLI, run `make prepare PROFILE=debug` (or `PROFILE=release` for release commands).

Native tab visibility tests launch an owned browser profile with its normal sandbox and no focus emulation. Linux CI uses GitHub-hosted Ubuntu 24.04 and selects packaged Chrome at `/opt/google/chrome/chrome`, covered by Ubuntu's existing application profile. It does not change host namespace or AppArmor settings. Engineering and release jobs free unused Android, .NET and Haskell SDK storage only on disposable GitHub-hosted Linux workers. `AGENTCHAT_NATIVE_BROWSER_BINARY` can select an explicit absolute executable for this test helper; otherwise it uses the prepared Playwright Chromium.

For a stabilized release, run the required whole Rust/Web/Mock/platform, dependency, generation, asset and build gates under their exact source/image scope. Keep original failed and partial runs; later focused fixes are qualified composition, not a rewritten clean whole run. See [verification](verification.md).

## Public file preparation

`python3 tools/public-files.py` emits the intended public source inventory. It is not a Git staging or publication command. Source checks retain their own broader engineering scope.

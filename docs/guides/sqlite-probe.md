# Disposable SQLite comparison

`agentchat-store` includes a small SeaORM-only example for comparing memory, WAL/FULL, WAL/NORMAL and bounded FULL batching on a new private one-table database. The diagnostic modes do not change the product's WAL/FULL connection policy.

Inspect the runner and use a new owned output directory:

```sh
python3 tools/run-sqlite-probe.py --help
```

The runner records actual commands, exits, platform metadata and phase timings. The example checks its effective connection settings, inserts/counts/reads rows and reopens disk-backed results. It never opens a real AgentChat project or changes a filesystem/device/global database setting. Results keep mode, row population and repetition boundaries.

Memory and NORMAL are diagnostic controls, not durable-performance acceptance. SQLite commit, Rust sync and platform fsync/full-sync APIs have distinct semantics; do not subtract their separate cohorts or claim equal power-loss durability from a successful call. Compare identical source/profile/fixture conditions and retain raw samples and failures. A slow persistence path is not by itself proof of SQLite computation, hardware firmware or filesystem compression as the single cause.

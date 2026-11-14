# Chigiri generator provenance

The Viewer contract uses published Chigiri 0.1.1 producer and runtime at `5f90235ddb705a1a394b9a5cfda23a9ea62dc4b6`, with generated ABI 8. `web/chigiri-provenance.json` binds the published host binary digests, clean full generator revision, exact runtime source, generation manifest, schema inputs, and owned output.

Run `make setup` to explicitly acquire the checksum-verified published generator for the current supported host. `python3 tools/chigiri_contract.py` checks complete ownership and canonical inputs. `python3 tools/chigiri_contract.py --generator PATH` additionally runs the qualified producer's supported `rust check`.

The Standards ownership adapter writes only `.craftguild/viewer-contract.json` from these verified canonical records. Its isolated reproduction creates a temporary schema/contract copy and runs Chigiri's independent rendering and exact comparison, preserving the recorded GeneratorInfo across hosts, before exporting the checked files. Only successfully verified generated Rust receives the narrow size exception; generated TypeScript remains measured. Never edit generated sources or ownership metadata manually. After an intentional schema or manifest change, use the pinned producer's `rust update`, then `python3 tools/chigiri_contract.py --record` to capture its actual output metadata.

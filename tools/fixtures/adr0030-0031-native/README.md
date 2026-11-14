# Historical native fixture controllers

These helpers retain the scoped ADR 0030/0031 native protocol and its offline regressions. They are not a current ACK-free ADR 0032 acceptance matrix. Do not replay an old actual-host campaign or reuse a former account/profile to refresh an evidence label. Current contracts are in the [native acceptance guide](../../../docs/guides/native-acceptance.md).

## Offline regression checks

Run `python3 -m unittest discover -s tools/tests -p 'test_native_matrix.py' -v` from the repository root. Tests use temporary synthetic inputs and do not grant authority to start a real native conversation.

## Explicit inputs and ownership

`inputs.example.json` illustrates the structured input shape. Supply a new owned case directory and exact AgentChat/native images, host versions, original owner facts and authorized isolated project/profile. Keep original commands/exits and unknown/failed/partial records; controller metadata cannot manufacture native acceptance or infer a process identity from a directory alone.

Normal host trust, approval, authentication and sandbox controls remain in force. Do not copy credentials, substitute HOME/CODEX_HOME, stop a shared server or alter global settings to obtain a passing case. Cleanup may target only positively identified owned helpers. See the [current delivery contract](../../../docs/design/delivery.md) before adapting any historical frame or receipt expectation.

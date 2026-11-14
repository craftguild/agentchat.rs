"""Inspect actual native MCP results without starting a host or granting authority.

Forked native transcripts can inherit old tool IDs and contexts. A transcript's
sessionId alone therefore never scopes a call to the current conversation.
The caller supplies a context established by separate native lifecycle proof.
"""

import argparse
import json
from pathlib import Path


class EvidenceError(ValueError):
    """A native transcript is inconsistent or lacks the required actual result."""


def _blocks(row):
    content = row.get("message", {}).get("content", [])
    return [v for v in content if isinstance(v, dict)] if isinstance(content, list) else []


def _envelope(value):
    return (isinstance(value, dict) and value.get("version") == 1
            and ("result" in value or "error" in value))


def read(path, root, limit=8 * 1024 * 1024):
    """Decode bounded raw tool-use/result pairs, retaining unknown outcomes.

    Structured native results can preserve the complete envelope when the
    display string is truncated. Conflicting actual envelopes or tool inputs
    are rejected. Neither an interrupted call nor an error proves a receipt.
    """
    with Path(path).open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise EvidenceError("Native transcript exceeds the bounded input limit")
    lines = [json.loads(line) for line in data.splitlines() if line]
    if any(row.get("sessionId") not in (None, root) for row in lines):
        raise EvidenceError("Native transcript contains a different root")
    uses = {}
    for row in lines:
        for block in _blocks(row):
            if block.get("type") != "tool_use":
                continue
            call = {k: block[k] for k in ("id", "name", "input")}
            previous = uses.setdefault(call["id"], call)
            if previous != call:
                raise EvidenceError("One native tool ID has conflicting inputs")
    results = []
    for row in lines:
        for block in _blocks(row):
            if block.get("type") != "tool_result":
                continue
            use = uses.get(block.get("tool_use_id"))
            if use is None or not use["name"].startswith("mcp__agentchat__"):
                continue
            result = {"tool_use_id": use["id"], "tool": use["name"],
                      "input": use["input"], "envelope": None,
                      "native_outcome_unknown": False}
            content = block.get("content")
            if isinstance(content, list):
                content = "".join(v.get("text", "") for v in content
                                  if isinstance(v, dict) and v.get("type") == "text")
            if isinstance(content, str) and content.startswith(
                    "[Tool call interrupted: the session ended before this call"):
                result["native_outcome_unknown"] = True
            else:
                candidates = []
                try:
                    decoded = json.loads(content)
                except (TypeError, ValueError):
                    decoded = None
                if _envelope(decoded):
                    candidates.append(decoded)
                structured = row.get("toolUseResult")
                if isinstance(structured, dict):
                    structured = structured.get("structuredContent", structured)
                if _envelope(structured):
                    candidates.append(structured)
                if candidates:
                    if any(v != candidates[0] for v in candidates):
                        raise EvidenceError("Native display and structured result disagree")
                    result["envelope"] = candidates[0]
                elif isinstance(content, str) and content.startswith("agentchat error summary: "):
                    summary, _ = json.JSONDecoder().raw_decode(content[len("agentchat error summary: "):])
                    result["envelope"] = {"request_id": summary["request_id"],
                                          "error": {k: v for k, v in summary.items()
                                                    if k != "request_id"}}
                else:
                    raise EvidenceError("Native MCP result has no complete envelope")
            result["native_error"] = block.get("is_error", False)
            # Identical inherited rows are history, not extra executions.
            if result not in results:
                results.append(result)
    return results


def context(record):
    """Return a call's exact input/binding context, refusing disagreement."""
    supplied = record["input"].get("context_id")
    result = (record.get("envelope") or {}).get("result", {})
    bound = result.get("binding", {}).get("context_id") if isinstance(result, dict) else None
    if supplied is not None and bound is not None and supplied != bound:
        raise EvidenceError("Native input and returned binding contexts disagree")
    return supplied if supplied is not None else bound


def scoped(records, context_id):
    """Select only the independently established context, including unbound status."""
    return [record for record in records if context(record) == context_id]


def _message(value, message_id, body):
    if isinstance(value, dict):
        if value.get("id") == message_id and value.get("body") == body:
            return True
        return any(_message(v, message_id, body) for v in value.values())
    return isinstance(value, list) and any(_message(v, message_id, body) for v in value)


def receipt(records, context_id, project_id, expected, earlier_body_contexts=()):
    """Require complete original body, exact delivery ACK and explicit read.

    This verifies transcript results only. Live root/owner proof, durable public
    state, policy preservation, native exits and profile admission are separate.
    """
    def successful(candidates):
        return [v for v in candidates
              if not v["native_error"] and v["envelope"] is not None
              and "result" in v["envelope"]
              and v["envelope"].get("project_id") == project_id]
    values = successful(scoped(records, context_id))
    mid, body = expected["message_id"], expected["body"]
    delivery_ids = set(expected["delivery_ids"])
    if not delivery_ids:
        raise EvidenceError("An original delivery ID is required")
    # A valid earlier retrieval survives ordinary resume. Earlier contexts
    # must be supplied explicitly; their ACK/read cannot satisfy current calls.
    body_values = values + [v for previous in earlier_body_contexts
                            for v in successful(scoped(records, previous))]
    full = [v for v in body_values if _message(v["envelope"]["result"], mid, body)]
    ack = [v for v in values if v["tool"] == "mcp__agentchat__agentchat_delivery_ack"
           and delivery_ids <= set(v["input"].get("delivery_ids", []))
           and delivery_ids <= set(v["envelope"]["result"].get("acknowledged", [])
                                   + v["envelope"]["result"].get("already_acknowledged", []))]
    read_result = [v for v in values if v["tool"] == "mcp__agentchat__agentchat_read"
                   and mid in v["input"].get("message_ids", [])
                   and mid in (v["envelope"]["result"].get("changed", [])
                               + v["envelope"]["result"].get("already_read", []))]
    if not full or not ack or not read_result:
        raise EvidenceError("Current context lacks actual full-body/ACK/read results")
    return {"full": full, "ack": ack, "read": read_result}


def main():
    """Inspect one retained transcript or verify an original-message receipt."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("transcript", type=Path)
    parser.add_argument("--root", required=True)
    parser.add_argument("--context", required=True)
    parser.add_argument("--project")
    parser.add_argument("--expected", type=Path)
    parser.add_argument("--earlier-body-context", action="append", default=[])
    args = parser.parse_args()
    if args.expected is not None and args.project is None:
        parser.error("--expected requires --project")
    records = read(args.transcript, args.root)
    result = (receipt(records, args.context, args.project, json.loads(args.expected.read_text()),
                      args.earlier_body_context)
              if args.expected else scoped(records, args.context))
    print(json.dumps({"context_id": args.context, "results": result,
                      "establishes_host_admission": False}, indent=2))


if __name__ == "__main__":
    main()

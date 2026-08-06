#!/usr/bin/env python3
"""Check nobox-agent's stdio output against MCP revision 2026-07-28.

The companion is checked the way a stock host would exercise it: discovery,
a deterministic tool list, one real tool call, and the two malformed requests
the revision requires a server to reject with specific codes.
"""

import json
import sys

VERSION = "2026-07-28"
LEGACY_VERSIONS = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]


def main(path: str) -> int:
    with open(path, encoding="utf-8") as stream:
        responses = {}
        for line in stream:
            if not line.strip():
                continue
            message = json.loads(line)
            responses[message.get("id")] = message

    # A notification must not be answered.
    if len(responses) != 6:
        print(f"expected six responses, got {sorted(responses)}", file=sys.stderr)
        return 1

    discover = responses[1]["result"]
    assert discover["resultType"] == "complete", discover
    assert discover["supportedVersions"] == [VERSION, *LEGACY_VERSIONS], discover
    assert "tools" in discover["capabilities"], discover
    server = discover["_meta"]["io.modelcontextprotocol/serverInfo"]
    assert server["name"] == "nobox-agent", server
    instructions = discover["instructions"]
    assert 0 < len(instructions.encode("utf-8")) <= 1_000, discover
    introduction = instructions[:512]
    for topic in (
        "permission-scoped",
        "desktop_snapshot",
        "desktop_subscribe",
        "resync_required",
        "client_capture",
    ):
        assert topic in introduction, (topic, introduction)

    listing = responses[2]["result"]
    names = [tool["name"] for tool in listing["tools"]]
    assert len(names) == len(set(names)), names
    # The revision asks for a deterministic order so clients can cache the list
    # and model prompts stay stable; two identical calls must agree exactly.
    assert responses[6]["result"]["tools"] == listing["tools"], names
    required = {
        "desktop_snapshot",
        "desktop_subscribe",
        "events_poll",
        "client_get",
        "client_semantic_root",
        "client_semantic_tree",
        "client_semantic_find",
        "seat_status",
    }
    assert required <= set(names), names
    assert isinstance(listing["ttlMs"], int), listing
    assert listing["cacheScope"], listing
    for tool in listing["tools"]:
        assert tool["inputSchema"]["type"] == "object", tool
        assert tool["description"], tool

    call = responses[3]["result"]
    assert call["resultType"] == "complete", call
    assert call["isError"] is False, call
    snapshot = call["structuredContent"]["snapshot"]
    titles = [client.get("title") for client in snapshot["clients"]]
    assert "nobox-agent-visible" in titles, titles
    assert "nobox-agent-secret" not in titles, titles
    assert isinstance(snapshot["sequence"], int), snapshot
    assert snapshot["workspaces"], snapshot
    assert snapshot["outputs"], snapshot

    # Missing per-request protocol fields are invalid params.
    assert responses[4]["error"]["code"] == -32602, responses[4]

    # A revision this server does not implement is refused by name.
    unsupported = responses[5]["error"]
    assert unsupported["code"] == -32022, unsupported
    assert unsupported["data"]["supported"] == [VERSION, *LEGACY_VERSIONS], unsupported
    assert unsupported["data"]["requested"] == "2025-11-25", unsupported

    print("MCP companion behaved as revision 2026-07-28 requires")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: agent-mcp-check.py OUTPUT.jsonl", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1]))

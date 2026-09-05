#!/usr/bin/env python3
"""Check two behavior-scoped connections on one Gents leader with live inference."""
import argparse
import json

from grok_edge_probe import LeaderClient, initialize
from grok_probe_common import graphql_escape, graphql_query


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--graphql", required=True)
    parser.add_argument("--cwd", required=True)
    parser.add_argument("--behavior", default="control-worker")
    args = parser.parse_args()
    clients = [LeaderClient(args.socket, 120, "GLM-5.3-Flash-NVFP4") for _ in range(2)]
    try:
        sessions = []
        owners = []
        for client, profile in zip(clients, [None, args.behavior]):
            session, _ = initialize(client, args.cwd, 524288, profile)
            sessions.append(session)
            response, _ = client.request("session/prompt", {"sessionId": session,
                "prompt": [{"type": "text", "text": "Reply exactly BEHAVIOR_OK. Do not use tools."}]})
            assert response.get("result", {}).get("stopReason") == "end_turn", response
            owner = graphql_query(args.graphql, '{AgentSession(filter:{session_id:{_eq:"' +
                graphql_escape(session) + '"}}){behavior_id}}')["AgentSession"]
            assert len(owner) == 1, owner
            owners.append(owner[0]["behavior_id"])
        assert owners[0] != owners[1] and owners[1] == args.behavior, owners
        for index, client in enumerate(clients):
            for method in ["x.ai/session/list", "x.ai/sessions/list"]:
                response, _ = client.request(method, {"limit": 1000})
                ids = [row["sessionId"] for row in response["result"]["sessions"]]
                assert sessions[index] in ids and sessions[1-index] not in ids, response
                literals = ",".join('"' + graphql_escape(value) + '"' for value in ids)
                rows = graphql_query(args.graphql, '{AgentSession(filter:{session_id:{_in:[' +
                    literals + ']}}){behavior_id}}')["AgentSession"]
                assert all(row["behavior_id"] == owners[index] for row in rows), rows
            denied, _ = client.request("session/load", {"sessionId": sessions[1-index], "cwd": args.cwd})
            assert "error" in denied, denied
        print(json.dumps({"result": "PASS", "sessions": sessions, "behaviors": owners,
            "checks": ["one listener/two bindings", "live inference", "both history filters", "cross-behavior load denied"]}))
    finally:
        for client in clients:
            client.close()


if __name__ == "__main__":
    main()

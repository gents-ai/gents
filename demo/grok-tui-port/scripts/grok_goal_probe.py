#!/usr/bin/env python3
"""Verify native goal hydration/controls on an explicitly isolated test server.

Creates one paused fixture goal in a new session (or an explicitly supplied
goal-free session). --keep-paused retains it for a stock TUI rendering check.
"""
import argparse
import json
import time
import uuid
from grok_edge_probe import LeaderClient, initialize
from grok_probe_common import graphql_query, graphql_escape


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--graphql", required=True)
    parser.add_argument("--cwd", required=True)
    parser.add_argument("--session")
    parser.add_argument("--keep-paused", action="store_true")
    args = parser.parse_args()
    client = LeaderClient(args.socket, 20, "GLM-5.3-Flash-NVFP4")
    try:
        if args.session:
            client.register()
            response, _ = client.request("initialize", {"protocolVersion":1, "clientCapabilities":{},
                "clientInfo":{"name":"goal-probe", "version":"1"}})
            assert "error" not in response, response
            session = args.session
            response, _ = client.request("session/load", {"sessionId":session, "cwd":args.cwd, "mcpServers":[]})
            assert "error" not in response, response
        else:
            session, _ = initialize(client, args.cwd, 524288)
        escaped = graphql_escape(session)
        owners = graphql_query(args.graphql, '{AgentSession(filter:{session_id:{_eq:"' + escaped + '"}}){agent_did requester_did}}')["AgentSession"]
        assert len(owners) == 1 and owners[0]["agent_did"] == owners[0]["requester_did"], owners
        principal = graphql_escape(owners[0]["agent_did"])
        goal_filter = 'agent_did:{_eq:"' + principal + '"},session_id:{_eq:"' + escaped + '"}'
        def goals():
            return graphql_query(args.graphql, '{Goal(filter:{' + goal_filter + '}){goal_id status tokens_used}}')["Goal"]
        assert not goals(), "Refusing to replace an existing goal"
        goal_id = "grok-goal-probe-" + uuid.uuid4().hex[:12]
        def seed():
            graphql_query(args.graphql, 'mutation{create_Goal(input:{goal_id:"' + goal_id + '",agent_did:"' + principal + '",session_id:"' + escaped + '",objective:"Goal panel fixture: verify native status and controls",status:"paused",token_budget:1000,tokens_used:123,active_time_seconds:7}){_docID}}')
        seed()
        observed = []
        for command, status in [("status", "paused"), ("pause", "paused"), ("clear", None)]:
            events = []
            response, _ = client.request("session/prompt", {"sessionId":session,
                "prompt":[{"type":"text", "text":"/goal " + command, "meta":{}}]}, events.append)
            assert response.get("result", {}).get("stopReason") == "end_turn", response
            assert any(event.get("params", {}).get("update", {}).get("_meta", {}).get("hostTurn") for event in events), events
            rows = goals()
            assert (rows[0]["status"] if rows else None) == status, rows
            observed.append(command)
            if command == "status":
                deadline = time.monotonic() + 10
                while not any(event.get("params", {}).get("update", {}).get("goal_id") == goal_id for event in events):
                    assert time.monotonic() < deadline, "No native goal update"
                    events.append(client.recv_acp())
                update = next(event["params"]["update"] for event in events if event.get("params", {}).get("update", {}).get("goal_id") == goal_id)
                assert update["status"] == "user_paused" and update["tokens_used"] == 123 and update["elapsed_ms"] == 7000, update
        if args.keep_paused:
            # Deleted Defra documents cannot be recreated with identical
            # content-addressed identity. Keep a distinct rendering fixture.
            goal_id += "-retained"
            seed()
        print(json.dumps({"result":"PASS", "session":session, "goal":goal_id,
            "commands":observed, "kept_paused":args.keep_paused,
            "resume":"unit-tested; deliberately not starting live inference in this wire probe"}))
    finally:
        client.close()


if __name__ == "__main__":
    main()

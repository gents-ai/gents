#!/usr/bin/env python3
"""Exercise real agent-originated updates using only native-e2e service labels."""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid


def run(argv, *, check=True, timeout=45):
    result = subprocess.run([str(arg) for arg in argv], capture_output=True, text=True,
                            timeout=timeout, env={**os.environ, "RUST_LOG": "error"})
    if check and result.returncode:
        raise AssertionError(f"{argv[0:3]} failed: {result.stderr}\n{result.stdout}")
    return result


def eventually(observe, seconds=60):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            last = observe()
            if last:
                return last
        except (OSError, ValueError, urllib.error.URLError, subprocess.TimeoutExpired):
            pass
        time.sleep(0.1)
    raise AssertionError(f"condition did not complete: {last}")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    metadata = json.loads(run([binary, "service", "update-check"]).stdout)
    label = metadata["service_label"]
    assert label.endswith(".native-e2e"), "refusing to address the daily runtime; build with --features native-e2e"
    system = metadata["os"]
    assert system in ("macos", "linux")
    if system == "macos":
        config_dir = Path.home() / "Library/LaunchAgents"
        definition = config_dir / f"{label}.plist"
    else:
        config_dir = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "systemd/user"
        definition = config_dir / metadata["systemd_unit"]
    assert not definition.exists(), f"existing native-e2e service must be removed externally first: {definition}"
    root = Path(tempfile.mkdtemp(prefix="gents-update-e2e-")).resolve()
    home = root / "node"
    home.mkdir()
    operation = str(uuid.uuid4())
    helper_label = f"{label}.update-{operation}"
    release = threading.Event()
    tool_result_seen = threading.Event()
    captured = []
    model = "managed-update-fixture"
    candidate = root / "candidate"
    shutil.copy2(binary, candidate)
    if system == "macos":
        run(["/usr/bin/codesign", "--force", "--sign", "-", "--identifier",
             f"ai.gents.update-fixture.{operation}", candidate])
    else:
        with candidate.open("ab") as stream:
            stream.write(b"\nmanaged-update-fixture\n")
    candidate_digest = digest(candidate)
    assert candidate_digest != digest(binary), "fixture must exercise a different executable build"
    update_args = ["service", "update", "--home", str(home), "--operation-id", operation,
                   "--executable", str(candidate), "--sha256", candidate_digest]

    class Provider(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            body = json.dumps({"object": "list", "data": [{"id": model, "object": "model", "owned_by": "fixture"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            captured.append(body)
            tools = body.get("tools") or []
            results = [message for message in body.get("messages", []) if message.get("role") == "tool"]
            if tools and not results:
                argv = ["service", "stop", "--home", str(home)]
                call_id = "unsafe-stop"
            elif tools and not any(operation in json.dumps(result) for result in results):
                assert any("disrupt the runtime" in json.dumps(result) for result in results), results
                argv = update_args
                call_id = "managed-update"
            else:
                argv = None
                if tools:
                    tool_result_seen.set()
                    assert release.wait(120), "test did not release initiating turn"
            if argv is not None:
                arguments = json.dumps({"command": str(binary), "args": argv, "timeout_secs": 60})
                chunks = [
                    {"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": call_id,
                         "type": "function", "function": {"name": "bash_unrestricted", "arguments": arguments}}]}, "finish_reason": None}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]},
                ]
            else:
                chunks = [
                    {"choices": [{"index": 0, "delta": {"content": "Update accepted; finishing this turn."}, "finish_reason": None}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 16, "completion_tokens": 8, "total_tokens": 24}},
                ]
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for chunk in chunks:
                self.wfile.write(b"data: " + json.dumps(chunk).encode() + b"\n\n")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()

    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    provider.daemon_threads = True
    thread = threading.Thread(target=provider.serve_forever, daemon=True)
    thread.start()
    http_port = socket.socket()
    http_port.bind(("127.0.0.1", 0))
    port = http_port.getsockname()[1]
    http_port.close()
    endpoint = f"http://127.0.0.1:{port}"
    installed = False
    succeeded = False

    def cli(*argv, check=True):
        return run([binary, *argv], check=check)

    def service_pid(helper=False):
        if system == "macos":
            target = f"gui/{os.getuid()}/{helper_label if helper else label}"
            response = run(["/bin/launchctl", "print", target], check=False)
            match = re.search(r"\bpid = (\d+)", response.stdout)
            return int(match.group(1)) if match else None
        unit = f"{helper_label}.service" if helper else metadata["systemd_unit"]
        response = run(["systemctl", "--user", "show", "--property=MainPID", "--value", unit], check=False)
        return int(response.stdout.strip()) or None

    def read_receipt():
        return json.loads((home / "service-updates" / operation / "receipt.json").read_text())

    def ready():
        with urllib.request.urlopen(f"{endpoint}/status", timeout=2) as response:
            value = json.load(response)
        return value if value.get("lifecycle") == "ready" else None

    try:
        init = json.loads(cli("init", "--home", home, "--identity-backend", "file", "--store-key-custody", "file",
            "--node-name", "update-fixture", "--yolo", "--tool-root", root,
            "--model-name", model, "--inference-url", f"http://127.0.0.1:{provider.server_port}/v1").stdout)
        env_file = root / "environment.json"
        env_file.write_text(json.dumps({"GENTS_UPDATE_TEST": "retained"}))
        cli("service", "install", "--home", home, "--executable", binary, "--http-addr", "127.0.0.1",
            "--http-port", port, "--p2p-bind-addr", "127.0.0.1", "--p2p-port", 0, "--environment-file", env_file)
        installed = True
        cli("service", "start", "--home", home)
        eventually(ready)
        initial_pid = eventually(service_pid)
        init_before = (home / "init.json").read_bytes()
        network_before = (home / "network.json").read_bytes()
        rejected = cli(*update_args[:-1], "0" * 64, check=False)
        assert rejected.returncode != 0 and "SHA-256 mismatch" in rejected.stderr
        assert service_pid() == initial_pid
        request = json.loads(cli("request", "submit", "--home", home, "--content", "Update your runtime and finish the turn.", "--no-wait").stdout)
        assert tool_result_seen.wait(90), "agent did not return the managed-update acceptance tool result"
        receipt = read_receipt()
        assert receipt["phase"] == "accepted", receipt
        assert receipt["request_id"] == request["request_id"], (receipt, request)
        assert receipt["session_id"], receipt
        assert ready() and service_pid() == initial_pid, "updater retired a busy initiating turn"
        duplicate = json.loads(cli(*update_args).stdout)
        assert duplicate["operation_id"] == operation and duplicate["phase"] == "accepted"
        first_helper = eventually(lambda: service_pid(helper=True))
        os.kill(first_helper, signal.SIGKILL)
        eventually(lambda: (pid := service_pid(helper=True)) and pid != first_helper, 25)
        assert read_receipt()["phase"] == "accepted"
        assert service_pid() == initial_pid, "helper recovery stopped unrelated active work"
        release.set()

        def completed():
            value = read_receipt()
            assert value["phase"] != "failed", value
            return value if value["phase"] == "healthy" else None

        final = eventually(completed, 180)
        assert service_pid() != initial_pid, "runtime was not replaced"
        assert (home / "init.json").read_bytes() == init_before
        assert (home / "network.json").read_bytes() == network_before
        assert "GENTS_UPDATE_TEST" in definition.read_text() and "retained" in definition.read_text()
        directory = home / "service-updates" / operation
        assert digest(directory / "gents") == candidate_digest
        assert digest(directory / "prior-gents") == digest(binary)
        assert (directory / "backup/init.json").read_bytes() == init_before
        assert not (directory / "backup/service-updates").exists()
        assert final["node_did"] == init["node_did"]
        status = json.loads(cli("service", "update-status", "--home", home, operation).stdout)
        assert status["phase"] == "healthy" and status["session_id"] == receipt["session_id"]
        stored = json.loads(cli("request", "show", "--home", home, "--output", "json", request["request_id"]).stdout)
        assert stored["request"]["lifecycle_state"] == "completed", stored
        assert stored["request"]["session_id"] == receipt["session_id"], stored
        final_pid = service_pid()
        cli("service", "update-run", "--home", home, operation)
        assert service_pid() == final_pid, "terminal helper replay changed the running runtime"
        succeeded = True
        print("PASS: agent self-stop refusal, verified replacement, acknowledgement before retirement, duplicate operation, helper crash recovery, cold backup, settings/identity preservation, and session outcome after reconnect")
    finally:
        release.set()
        if system == "macos":
            run(["/bin/launchctl", "bootout", f"gui/{os.getuid()}/{helper_label}"], check=False)
        else:
            run(["systemctl", "--user", "stop", f"{helper_label}.service"], check=False)
            (config_dir / f"{helper_label}.service").unlink(missing_ok=True)
            run(["systemctl", "--user", "daemon-reload"], check=False)
        if installed:
            cli("service", "uninstall", "--home", home, check=False)
        provider.shutdown()
        if succeeded:
            shutil.rmtree(root)
        else:
            print(f"Fixture retained for diagnosis: {root}", file=sys.stderr)


if __name__ == "__main__":
    main()

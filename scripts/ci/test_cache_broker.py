#!/usr/bin/env python3
"""Run Lake outside the traced process tree during a traced test slice.

  test_cache_broker.py serve SOCKET PROGRAM   run PROGRAM for every call on SOCKET
  test_cache_broker.py call SOCKET ARG...     run PROGRAM ARG... through the broker

The tracer stops a process at every file system call; Lake checks tens of
thousands of build files each time a test runs it, which made a traced run
about 2.8 times an untraced one. Its output is keyed by the proofs' sources
(`tree:crates/gents/proofs`, the derived rule in scripts/ci/test_cache.py), so
its own reads need not be traced: a test's `lake` is a shim that hands its
arguments, directory, environment and standard descriptors to this broker,
which the slice started before the tracer, and returns Lake's exit status. The
shim's execution is the test's query of the proofs' sources.
"""

import array
import json
import os
import signal
import socket
import subprocess
import sys
import threading


def serve(path, program):
    if os.path.exists(path):
        os.unlink(path)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(path)
    server.listen(64)
    while True:
        connection, _ = server.accept()
        threading.Thread(target=handle, args=(connection, program), daemon=True).start()


def handle(connection, program):
    with connection:
        message, fds, _, _ = socket.recv_fds(connection, 1 << 20, 3)
        request = json.loads(message)
        try:
            child = subprocess.Popen([program, *request["arguments"]], cwd=request["cwd"], env=request["env"],
                                     stdin=fds[0], stdout=fds[1], stderr=fds[2], start_new_session=True)
        except OSError as error:
            connection.sendall(json.dumps({"status": 127, "error": str(error)}).encode() + b"\n")
            return
        finally:
            for fd in fds:
                os.close(fd)

        def watch():
            # The caller died (a test timed out and killed its children): stop Lake too.
            try:
                ended = not connection.recv(1)
            except OSError:
                return  # the reply was sent and the connection closed
            if ended:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        threading.Thread(target=watch, daemon=True).start()
        status = child.wait()
        status = 128 - status if status < 0 else status
        try:
            connection.sendall(json.dumps({"status": status}).encode() + b"\n")
            # Wakes the watcher: a close alone leaves its pending receive holding
            # the socket open, and the caller would wait for the end forever.
            connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


def call(path, arguments):
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.connect(path)
    request = {"arguments": arguments, "cwd": os.getcwd(), "env": dict(os.environ)}
    socket.send_fds(connection, [json.dumps(request).encode()], [0, 1, 2])
    reply = b""
    while not reply.endswith(b"\n") and (chunk := connection.recv(4096)):
        reply += chunk
    response = json.loads(reply or b'{"status": 1}')
    if response.get("error"):
        print(f"test_cache_broker: {response['error']}", file=sys.stderr)
    sys.exit(response["status"])


def shim(directory, socket_path, name="lake"):
    """Write `<directory>/<name>`, a program that runs `name` through the broker."""
    os.makedirs(directory, exist_ok=True)
    target = os.path.join(directory, name)
    with open(target, "w") as handle:
        handle.write(f'#!/bin/sh\nexec "{sys.executable}" "{os.path.abspath(__file__)}" call "{socket_path}" "$@"\n')
    os.chmod(target, 0o755)
    return target


if __name__ == "__main__":
    if len(sys.argv) >= 4 and sys.argv[1] == "serve":
        serve(sys.argv[2], sys.argv[3])
    elif len(sys.argv) >= 3 and sys.argv[1] == "call":
        call(sys.argv[2], sys.argv[3:])
    else:
        sys.exit(__doc__)

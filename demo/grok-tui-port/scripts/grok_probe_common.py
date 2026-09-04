"""Shared Unix-socket support for the checked-in Grok live probes."""

from __future__ import annotations

import os
import socket
import stat
import tempfile
from pathlib import Path


PORTABLE_UNIX_PATH_BYTES = 100


class PortableSocketPath:
    """A short private alias for a long, identity-bearing Unix socket path."""

    def __init__(self, path: str) -> None:
        self.identity = Path(path).resolve()
        self.alias_dir: Path | None = None
        self.alias_path: Path | None = None
        self.connect_path = str(self.identity)
        if len(os.fsencode(self.connect_path)) <= PORTABLE_UNIX_PATH_BYTES:
            return

        alias_dir = Path(tempfile.mkdtemp(prefix=".gents-grok-client-", dir="/tmp"))
        alias = alias_dir / "s"
        try:
            mode = stat.S_IMODE(alias_dir.stat().st_mode)
            if mode != 0o700:
                raise AssertionError(f"private socket-alias directory has mode {mode:o}")
            alias.symlink_to(self.identity)
            if len(os.fsencode(str(alias))) > PORTABLE_UNIX_PATH_BYTES:
                raise AssertionError(
                    "private leader-socket alias exceeds the portable Unix path limit"
                )
        except BaseException:
            alias.unlink(missing_ok=True)
            alias_dir.rmdir()
            raise
        self.alias_dir = alias_dir
        self.alias_path = alias
        self.connect_path = str(alias)

    def cleanup(self) -> None:
        if self.alias_dir is None:
            return
        if self.alias_path is None:
            raise AssertionError("socket-alias path is missing for its private directory")
        self.alias_path.unlink(missing_ok=True)
        self.alias_dir.rmdir()
        self.alias_path = None
        self.alias_dir = None


def self_test_portable_socket_path() -> None:
    """Prove an over-limit published socket works through the private alias."""
    with tempfile.TemporaryDirectory(prefix="gents-socket-test-", dir="/tmp") as root_text:
        root = Path(root_text)
        long_dir = root / ("x" * 88)
        long_dir.mkdir()
        staged = root / "b"
        published = long_dir / "leader.sock"
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        accepted: socket.socket | None = None
        bridge: PortableSocketPath | None = None
        try:
            listener.bind(str(staged))
            listener.listen(1)
            os.rename(staged, published)
            if len(os.fsencode(str(published.resolve()))) <= PORTABLE_UNIX_PATH_BYTES:
                raise AssertionError("portable socket fixture is not over the absolute path limit")
            bridge = PortableSocketPath(str(published))
            if not Path(bridge.connect_path).is_absolute():
                raise AssertionError("socket alias is not absolute")
            client.connect(bridge.connect_path)
            accepted, _ = listener.accept()
            if Path(bridge.connect_path).resolve() != published.resolve():
                raise AssertionError("alias identity drifted")
        finally:
            if accepted is not None:
                accepted.close()
            client.close()
            listener.close()
            if bridge is not None:
                bridge.cleanup()

"""Shared Unix-socket support for the checked-in Grok live probes."""

from __future__ import annotations

import os
import socket
import stat
import tempfile
from pathlib import Path


PORTABLE_UNIX_PATH_BYTES = 100


def _new_private_socket_leaf(
    prefix: str, leaf_name: str
) -> tuple[tempfile.TemporaryDirectory, Path]:
    """Honor the configured temp directory, falling back when its path is too long."""
    configured = Path(tempfile.gettempdir())
    fallback = Path("/tmp")
    parents = [configured]
    if configured != fallback:
        parents.append(fallback)

    for parent in parents:
        owner = tempfile.TemporaryDirectory(prefix=prefix, dir=parent)
        directory = Path(owner.name)
        mode = stat.S_IMODE(directory.stat().st_mode)
        if mode != 0o700:
            owner.cleanup()
            raise AssertionError(f"private socket directory has mode {mode:o}")
        leaf = directory / leaf_name
        if len(os.fsencode(str(leaf))) <= PORTABLE_UNIX_PATH_BYTES:
            return owner, leaf
        owner.cleanup()

    raise OSError("no temporary directory yields a portable Unix socket path")


class PortableSocketPath:
    """A short private alias for a long, identity-bearing Unix socket path."""

    def __init__(self, path: str) -> None:
        self.identity = Path(path).resolve()
        self.alias_owner: tempfile.TemporaryDirectory | None = None
        self.alias_path: Path | None = None
        self.connect_path = str(self.identity)
        if len(os.fsencode(self.connect_path)) <= PORTABLE_UNIX_PATH_BYTES:
            return

        alias_owner, alias = _new_private_socket_leaf(".gents-grok-client-", "s")
        try:
            alias.symlink_to(self.identity)
        except BaseException:
            alias_owner.cleanup()
            raise
        self.alias_owner = alias_owner
        self.alias_path = alias
        self.connect_path = str(alias)

    def cleanup(self) -> None:
        if self.alias_owner is None:
            return
        if self.alias_path is None:
            raise AssertionError("socket-alias path is missing for its private directory")
        self.alias_owner.cleanup()
        self.alias_path = None
        self.alias_owner = None


def self_test_portable_socket_path() -> None:
    """Prove an over-limit published socket works through the private alias."""
    root_owner, staged = _new_private_socket_leaf("gents-socket-test-", "b")
    with root_owner as root_text:
        root = Path(root_text)
        long_dir = root / ("x" * 88)
        published = long_dir / "leader.sock"
        long_dir.mkdir()
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener, socket.socket(
            socket.AF_UNIX, socket.SOCK_STREAM
        ) as client:
            listener.bind(str(staged))
            listener.listen(1)
            os.rename(staged, published)
            if len(os.fsencode(str(published.resolve()))) <= PORTABLE_UNIX_PATH_BYTES:
                raise AssertionError(
                    "portable socket fixture is not over the absolute path limit"
                )
            bridge = PortableSocketPath(str(published))
            try:
                if not Path(bridge.connect_path).is_absolute():
                    raise AssertionError("socket alias is not absolute")
                client.connect(bridge.connect_path)
                accepted, _ = listener.accept()
                with accepted:
                    if Path(bridge.connect_path).resolve() != published.resolve():
                        raise AssertionError("alias identity drifted")
            finally:
                bridge.cleanup()

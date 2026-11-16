#!/usr/bin/env python3
"""The gate's test-result cache for archived test runs.

crates/gents/proofs/Proofs/Ci/TestCache.lean specifies it: `decide` and `observe`
here implement the model's functions of the same names, and
test_test_cache.py checks them against the model's own execution. The premises
the model's soundness theorem rests on are enforced here:

  (a) determinism: tests listed in .config/test-cache.toml always run;
  (b) complete key: `static_key` holds every non-file input; the tracer
      records every path a test observes, and an access it cannot classify
      keeps the test from being recorded;
  (c) no outside world: non-loopback connections, name resolution and the
      service manager keep a test from being recorded;
  (d) trust: only runs with GATE_TEST_CACHE_TRUSTED=true record.

A record is one file, published by rename; contradictions are never removed
(crates/gents/proofs/tla/TestCacheStore.tla). The store is a directory on the runner host; the record's
fields are those a signed result document would carry.
"""

import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import xml.etree.ElementTree as ElementTree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
POLICY_FILE = ROOT / ".config" / "test-cache.toml"
SCHEMA = 1
RETENTION_DAYS = 14

# ---- the model's decision ----


def agrees(answer, trace):
    return all(answer(path) == expected for path, expected in trace)


def vouches(answer, test, key, record):
    return record["test"] == test and record["key"] == key and agrees(answer, record["trace"])


def decide(policy, store, test, key, answer):
    """TestCache.lean `decide`: ("skip", record) or ("run", reason)."""
    if policy["full"]:
        return ("run", "full")
    if test in policy["excluded"]:
        return ("run", "excluded")
    if [test, key] in store["contradicted"]:
        return ("run", "contradicted")
    for record in store["records"]:
        if vouches(answer, test, key, record):
            return ("skip", record)
    return ("run", "miss")


def vouched(policy, execution):
    return (execution["trusted"] and execution["traceComplete"] and execution["checkoutClean"]
            and execution["test"] not in policy["excluded"])


def contradicts(policy, execution, record):
    return (not execution["passed"] and vouched(policy, execution)
            and vouches(execution["fs"], execution["test"], execution["key"], record))


def observe(policy, store, execution):
    """TestCache.lean `observe`: the store after one executed test."""
    if execution["passed"] and vouched(policy, execution):
        record = {"test": execution["test"], "key": execution["key"], "trace": execution["trace"],
                  "run": execution["run"]}
        return {"records": [record] + store["records"], "contradicted": store["contradicted"]}
    if any(contradicts(policy, execution, record) for record in store["records"]):
        return {"records": store["records"],
                "contradicted": [[execution["test"], execution["key"]]] + store["contradicted"]}
    return store


# ---- policy ----


def load_policy(path=POLICY_FILE):
    # Imported here: gate.py imports this module on the Studios, whose Python
    # predates tomllib (3.11), and only the Linux cache path reads the policy.
    import tomllib
    data = tomllib.loads(path.read_text())
    excluded, binaries = {}, {}
    for entry in data.get("exclude", []):
        if not entry.get("issue") or not entry.get("reason"):
            raise ValueError(f"{path}: every exclusion names an issue and a reason: {entry}")
        if "test" in entry:
            binary, _, name = entry["test"].partition(" ")
            if not name:
                raise ValueError(f"{path}: `test` is '<binary-id> <test name>': {entry['test']!r}")
            excluded[(binary, name)] = entry
        elif "binary" in entry:
            binaries[entry["binary"]] = entry
        else:
            raise ValueError(f"{path}: an exclusion names a `test` or a `binary`: {entry}")
    environment = data.get("environment", {})
    return {
        "version": data["version"],
        "excluded": excluded,
        "excluded_binaries": binaries,
        "env_names": set(environment.get("names", [])),
        "env_prefixes": tuple(environment.get("prefixes", [])),
        "env_ignored_prefixes": tuple(environment.get("ignored_prefixes", [])),
    }


def excluded(policy, binary, name):
    """The exclusion entry covering a test, or None."""
    return policy["excluded"].get((binary, name)) or policy["excluded_binaries"].get(binary)


def keyed_environment(policy, environ):
    keyed = {}
    for name, value in environ.items():
        if name.startswith(policy["env_ignored_prefixes"]):
            continue
        if name in policy["env_names"] or name.startswith(policy["env_prefixes"]):
            keyed[name] = value
    return dict(sorted(keyed.items()))


def static_key(policy, binary_id, binary_digest, runner):
    """TestCache.lean `StaticKey`: every non-file input of one test binary.

    `runner` holds the nextest version, the digest of its configuration and the
    keyed environment, which are the same for every binary of a run.
    """
    components = {"schema": SCHEMA, "policy": policy["version"], "binary": binary_id,
                  "digest": binary_digest, **runner}
    return hashlib.sha256(json.dumps(components, sort_keys=True).encode()).hexdigest()


# ---- answers: what a path holds now ----


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


class Answers:
    """The current answer to every query in the gate's path namespace.

    ws:<path>    the commit's tracked tree (mode and blob id; directories list tracked entries)
    tree:<dir>   the commit's tree id of a directory whose derived build output a test read
    target:<p>   a file of the extracted test archive, by SHA-256
    host:<p>     a file on this host (`~` is the account's home), by SHA-256
    up:<p>       an ancestor directory of the checkout, by kind only
    """

    def __init__(self, root, extract=None, home=None):
        self.root = Path(root)
        self.extract = Path(extract) if extract else None
        self.home = home or os.path.expanduser("~")
        self.memo = {}
        listing = subprocess.run(["git", "ls-files", "-s", "-z"], cwd=self.root, check=True,
                                 stdout=subprocess.PIPE).stdout.decode()
        self.tracked, self.children = {}, {}
        for entry in filter(None, listing.split("\0")):
            meta, path = entry.split("\t", 1)
            mode, blob, _ = meta.split()
            self.tracked[path] = f"{mode}:{blob}"
            parts = path.split("/")
            for depth in range(len(parts)):
                parent = "/".join(parts[:depth])
                self.children.setdefault(parent, set()).add(parts[depth])

    def __call__(self, path):
        if path not in self.memo:
            self.memo[path] = self.compute(path)
        return self.memo[path]

    def compute(self, path):
        kind, _, rest = path.partition(":")
        if kind == "ws":
            if rest == ".git":
                return {"file": "repository"} if (self.root / ".git").exists() else "absent"
            if rest in self.tracked:
                return {"file": self.tracked[rest]}
            if rest in self.children:
                return {"dir": sorted(self.children[rest])}
            return "absent"
        if kind == "tree":
            tree = subprocess.run(["git", "rev-parse", f"HEAD:{rest}"], cwd=self.root,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
            return {"file": tree.stdout.strip()} if tree.returncode == 0 else "absent"
        if kind == "target":
            base = self.extract
            if base is None:
                return "absent"
            # A directory of the archive is keyed by kind: a build script's
            # OUT_DIR is archived whole, and with kept target trees it holds
            # each runner's leftovers (dispatches 43788390725 and 43791453932
            # built identical binaries and every listing differed). The
            # loader and the tests probe the files they need, each a query.
            answer = self.disk(base / rest)
            return {"dir": []} if isinstance(answer, dict) and "dir" in answer else answer
        if kind == "up":
            target = Path(rest)
            return {"dir": []} if target.is_dir() else ({"file": "exists"} if target.exists() else "absent")
        if kind == "host":
            return self.disk(Path(self.home + rest[1:] if rest.startswith("~") else rest))
        raise ValueError(f"unknown path namespace in {path!r}")

    @staticmethod
    def disk(path):
        try:
            if path.is_dir():
                return {"dir": sorted(entry.name for entry in path.iterdir())}
            if path.exists():
                return {"file": sha256_file(path)}
        except OSError:
            return "unreadable"
        return "absent"


# ---- the tracer: what each test observed ----

TRACE_SYSCALLS = ",".join([
    "%file", "%process", "fchdir", "connect",
])

LINE = re.compile(r"^(\d+)\s+(.*)$")
CALL = re.compile(r"^([a-z0-9_]+)\((.*)\)\s+=\s+(.*)$", re.S)
DECORATION = re.compile(r"^(?:AT_FDCWD|-?\d+)<(.*)>$")
WRITE_FLAGS = ("O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND")
WRITES = {"rename", "renameat", "renameat2", "unlink", "unlinkat", "mkdir", "mkdirat", "rmdir",
          "symlink", "symlinkat", "link", "linkat", "truncate", "creat", "chmod", "fchmodat",
          "chown", "fchownat", "lchown", "utimensat", "mknod", "mknodat"}
STATS = {"stat", "lstat", "newfstatat", "fstatat64", "statx", "access", "faccessat", "faccessat2",
         "readlink", "readlinkat", "getxattr", "lgetxattr", "listxattr", "llistxattr", "statfs",
         "inotify_add_watch", "fanotify_mark", "name_to_handle_at", "chroot", "utime", "utimes"}
# Files in the account's home that are host configuration, keyed by digest like
# /usr and /etc. Anything else there is the user's state and refuses the record.
HOME_INPUTS = (".elan", ".rustup", ".cargo", ".local/bin", ".cache/cargo-nextest", "gents-build/.guard",
               ".gitconfig", ".config/git", ".local/lib")
# Tool bookkeeping in the home that tools write as they run and that changes no
# result: elan appends every directory it has run in, so with several checkouts
# on a host it changed between runs and invalidated every Lean-backed test.
HOME_SCRATCH = (".elan/known-projects",)
SERVICE_MANAGERS = ("systemctl", "launchctl", "loginctl", "busctl", "dbus-send", "sudo")
# Name resolution is the network even when the resolver listens on loopback.
RESOLVER = {("127.0.0.53", 53), ("127.0.0.54", 53)}


def unquote(text):
    """A strace string argument, without quotes and escapes."""
    if not text.startswith('"'):
        return None
    out, index = bytearray(), 1
    while index < len(text):
        char = text[index]
        if char == '"':
            return out.decode("utf-8", "surrogateescape")
        if char == "\\":
            following = text[index + 1]
            if following in "01234567":
                digits = re.match(r"[0-7]{1,3}", text[index + 1:]).group(0)
                out.append(int(digits, 8))
                index += 1 + len(digits)
                continue
            if following == "x":
                out.append(int(text[index + 2:index + 4], 16))
                index += 4
                continue
            out += {"n": b"\n", "t": b"\t", "r": b"\r", "v": b"\v", "f": b"\f"}.get(following, following.encode())
            index += 2
            continue
        out += char.encode("utf-8", "surrogateescape")
        index += 1
    return None


def split_arguments(text):
    """Top-level comma-separated arguments of a strace call."""
    arguments, depth, start, index, quoted = [], 0, 0, 0, False
    while index < len(text):
        char = text[index]
        if quoted:
            if char == "\\":
                index += 1
            elif char == '"':
                quoted = False
        elif char == '"':
            quoted = True
        elif char in "([{<":
            depth += 1
        elif char in ")]}>":
            depth -= 1
        elif char == "," and depth == 0:
            arguments.append(text[start:index].strip())
            start = index + 1
        index += 1
    tail = text[start:].strip()
    if tail:
        arguments.append(tail)
    return arguments


def decoration(argument):
    match = DECORATION.match(argument or "")
    return match.group(1) if match else None


class Context:
    """Where a run's paths belong: the checkout, the extracted archive, scratch
    space, the host, and build output keyed by its sources."""

    def __init__(self, root, extract, home=None, scratch=(), derived=(), refused=()):
        self.root = os.path.normpath(root)
        self.extract = os.path.normpath(extract) if extract else None
        self.home = os.path.normpath(home or os.path.expanduser("~"))
        self.scratch = tuple(os.path.normpath(path) for path in scratch)
        # (absolute prefix, source tree): build output a test reads, keyed by its sources.
        self.derived = tuple((os.path.normpath(prefix), tree) for prefix, tree in derived)
        self.refused = tuple(os.path.normpath(path) for path in refused)

    @staticmethod
    def within(path, prefix):
        return path == prefix or path.startswith(prefix.rstrip("/") + "/")

    def classify(self, path):
        """(kind, normalized path or reason): kind is input, scratch or refused."""
        path = os.path.normpath(path)
        for prefix in self.refused:
            if self.within(path, prefix):
                return ("refused", f"reads {path}, outside the run's inputs")
        for prefix, tree in self.derived:
            if self.within(path, prefix):
                return ("input", f"tree:{tree}")
        if self.extract and self.within(path, self.extract):
            return ("input", "target:" + os.path.relpath(path, self.extract))
        if self.within(path, self.root):
            relative = os.path.relpath(path, self.root)
            if relative == ".":
                return ("input", "ws:")
            if self.within(relative, "target/nextest"):
                return ("scratch", None)
            if relative == ".git":
                return ("input", "ws:.git")  # the search for a repository root
            if self.within(relative, "target") or self.within(relative, ".git"):
                return ("refused", f"reads {relative} in the checkout, which is not the commit's content")
            return ("input", "ws:" + relative)
        for prefix in ("/proc", "/sys", "/dev", "/tmp", "/var/tmp", "/run/user") + self.scratch:
            if self.within(path, prefix):
                return ("scratch", None)
        for name in HOME_SCRATCH:
            if path == os.path.join(self.home, name):
                return ("scratch", None)
        for prefix in (os.path.join(self.home, name) for name in HOME_INPUTS):
            if self.within(path, prefix):
                return ("input", "host:~" + path[len(self.home):])
        if any(self.within(base, path) for base in (self.root, self.extract, *(p for p, _ in self.derived)) if base):
            # An ancestor of the checkout, probed while searching for the
            # workspace root: its kind matters, not its siblings, which are
            # other checkouts on the runner. Markers probed beside it are their
            # own queries.
            return ("input", "up:" + path)
        if self.within(path, self.home):
            return ("refused", f"reads {path} in the account's home")
        # Anything else on the host is keyed by its content, like /usr and /etc.
        return ("input", "host:" + path)


class TestTrace:
    def __init__(self, binary_path, name):
        self.binary_path = binary_path
        self.name = name
        self.queries = {}   # normalized path -> kind observed: file, dir, absent, any
        self.problems = []
        self.wrote = set()  # checkout paths this test's processes wrote

    def query(self, path, kind):
        if path.startswith("tree:") or path == "ws:.git":
            kind = "any"
        previous = self.queries.get(path)
        if previous is None or previous == "any":
            self.queries[path] = kind
        elif kind != "any" and previous != kind:
            self.problems.append(f"{path} was both {previous} and {kind} during the test")

    def refuse(self, reason):
        if reason not in self.problems and len(self.problems) < 20:
            self.problems.append(reason)


STRUCTURE = ("clone", "clone3", "fork", "vfork", "execve", "execveat", "chdir", "fchdir")


def call_parts(text):
    """(name, arguments, result) of one complete strace line, or None."""
    call = CALL.match(text)
    if not call:
        return None
    return call.group(1), call.group(2), call.group(3)


def test_exec(name, arguments, result, cwd, test_binaries):
    """The (binary, test name) a successful execve starts, or None."""
    if name not in ("execve", "execveat") or result.startswith("-1"):
        return None
    arguments = split_arguments(arguments)
    directory = decoration(arguments[0]) if name == "execveat" else None
    path = unquote(arguments[1] if name == "execveat" else arguments[0])
    if path is None:
        return None
    if not path.startswith("/"):
        base = directory or cwd
        if not base:
            return None
        path = os.path.join(base, path)
    if os.path.realpath(path) not in test_binaries:
        return None
    argv = re.findall(r'"((?:[^"\\]|\\.)*)"', arguments[2 if name == "execveat" else 1])
    if "--exact" not in argv or argv.index("--exact") + 1 >= len(argv):
        return None
    return (os.path.realpath(path), unquote('"' + argv[argv.index("--exact") + 1] + '"'))


def process_tree(structure, test_binaries):
    """From each process's clone, exec and chdir lines (in order per process):
    the test that owns a process when it starts, and its starting directory."""
    events = {}  # pid -> [("clone", child) | ("test", key) | ("cwd", path)]
    parents, pending = {}, {}
    for pid, text in structure:
        if text.endswith("<unfinished ...>"):
            pending[pid] = text[: -len("<unfinished ...>")].rstrip()
            continue
        if text.startswith("<..."):
            resumed = re.match(r"<\.\.\. [a-z0-9_]+ resumed>\s?(.*)$", text, re.S)
            if not resumed or pid not in pending:
                continue
            text = pending.pop(pid) + resumed.group(1)
        parts = call_parts(text)
        if not parts:
            continue
        name, arguments, result = parts
        log = events.setdefault(pid, [])
        if name in ("clone", "clone3", "fork", "vfork"):
            child = re.match(r"\s*(\d+)", result)
            if child:
                log.append(("clone", int(child.group(1))))
                parents[int(child.group(1))] = pid
        elif name in ("chdir", "fchdir") and not result.startswith("-1"):
            argument = split_arguments(arguments)[0]
            target = decoration(argument) if name == "fchdir" else unquote(argument)
            if target:
                log.append(("cwd", target))
        else:
            key = test_exec(name, arguments, result, None, test_binaries)
            if key:
                log.append(("test", key))

    start = {}  # pid -> (owner at start, cwd at start)

    def started(pid):
        if pid in start:
            return start[pid]
        start[pid] = (None, None)
        parent = parents.get(pid)
        if parent is None:
            return start[pid]
        owner, cwd = started(parent)
        for kind, value in events.get(parent, []):
            if kind == "clone" and value == pid:
                break
            if kind == "test":
                owner = value
            elif kind == "cwd":
                cwd = value if value.startswith("/") or cwd is None else os.path.join(cwd, value)
        start[pid] = (owner, cwd)
        return start[pid]

    for pid in set(events) | set(parents):
        started(pid)
    return start


def parse_trace(records, context, test_binaries, derived_pids=(), derived_tree=None):
    """Per-test queries from a `strace -ff -y` trace of one nextest run.

    `records` returns, each time it is called, an iterator of (pid, line), each
    process's lines in order. A process that executes a test binary with
    `--exact <name>` runs that test from then on, and every process it starts
    belongs to it. `derived_pids` are processes whose lines the prefilter
    dropped because they read derived build output (Lake's): each is a query
    of `derived_tree`. Returns the traces by (binary path, test name) and, for
    each checkout path written during the run, the tests that wrote it (None
    for a process outside every test).
    """
    def structural(text):
        name = text[5:].split(" ", 1)[0] if text.startswith("<... ") else text.split("(", 1)[0]
        return name in STRUCTURE

    start = process_tree(((pid, text) for pid, text in records() if structural(text)), test_binaries)
    tests, written = {}, {}
    owner, cwd, pending = {}, {}, {}

    def trace_of(pid):
        key = owner.get(pid)
        return tests.get(key) if key else None

    def resolve(pid, directory, path):
        if path is None:
            return None
        if path.startswith("/"):
            return path
        base = directory or cwd.get(pid)
        return os.path.join(base, path) if base else None

    def read(pid, path, kind):
        trace = trace_of(pid)
        if trace is None:
            return
        if path is None:
            trace.refuse("a relative path from an unknown directory")
            return
        category, value = context.classify(path)
        if category == "refused" and kind == "absent" and context.within(os.path.normpath(path), context.home):
            # A probe that found nothing (a PATH search through a runner's tool
            # cache): the absence is the observation, keyed like any host path.
            trace.query("host:~" + os.path.normpath(path)[len(context.home):], "absent")
        elif category == "refused":
            trace.refuse(value)
        elif category == "input":
            trace.query(value, kind)

    def write(pid, path):
        if path is None:
            return
        category, value = context.classify(path)
        trace = trace_of(pid)
        if category == "input" and value.startswith("ws:"):
            written.setdefault(value, set()).add(owner.get(pid))
            if trace:
                trace.wrote.add(value)
        elif category == "refused" and trace:
            trace.refuse(value.replace("reads", "writes", 1))

    for pid, text in records():
        if pid not in owner and pid not in cwd:
            owner[pid], cwd[pid] = start.get(pid, (None, None))
            if owner[pid] and owner[pid] not in tests:
                tests[owner[pid]] = TestTrace(*owner[pid])
        if text.startswith(("+++", "---")):
            continue
        if text.endswith("<unfinished ...>"):
            pending[pid] = text[: -len("<unfinished ...>")].rstrip()
            continue
        if text.startswith("<..."):
            resumed = re.match(r"<\.\.\. [a-z0-9_]+ resumed>\s?(.*)$", text, re.S)
            if not resumed or pid not in pending:
                continue
            text = pending.pop(pid) + resumed.group(1)
        parts = call_parts(text)
        if not parts:
            continue
        name, raw_arguments, result = parts
        if name in ("clone", "clone3", "fork", "vfork", "getcwd", "exit", "exit_group", "wait4"):
            continue
        failed = result.startswith("-1")
        errno = result.split()[1] if failed and len(result.split()) > 1 else None
        returned = None if failed else decoration(result.split(" ", 1)[0])
        arguments = split_arguments(raw_arguments)

        if name in ("chdir", "fchdir"):
            if not failed:
                target = decoration(arguments[0]) if name == "fchdir" else resolve(pid, None, unquote(arguments[0]))
                if target:
                    cwd[pid] = target
            continue
        if name in ("execve", "execveat"):
            key = test_exec(name, raw_arguments, result, cwd.get(pid), test_binaries)
            if key:
                owner[pid] = key
                tests.setdefault(key, TestTrace(*key))
                continue  # the test binary itself is in the static key
            trace = trace_of(pid)
            directory = decoration(arguments[0]) if name == "execveat" else None
            path = resolve(pid, directory, unquote(arguments[1] if name == "execveat" else arguments[0]))
            if trace and path:
                program = os.path.basename(path)
                if not failed and program in SERVICE_MANAGERS:
                    trace.refuse(f"executes {program}: the service manager")
                read(pid, path, "absent" if errno in ("ENOENT", "ENOTDIR") else ("file" if not failed else "any"))
            continue
        if name == "connect":
            trace = trace_of(pid)
            if trace is None:
                continue
            address = arguments[1] if len(arguments) > 1 else ""
            family = re.search(r"sa_family=(AF_[A-Z0-9]+)", address)
            family = family.group(1) if family else ""
            if family in ("AF_INET", "AF_INET6"):
                host = re.search(r'inet_addr\("([^"]+)"\)|inet_pton\(AF_INET6, "([^"]+)"', address)
                port = re.search(r"htons\((\d+)\)", address)
                host = (host.group(1) or host.group(2)) if host else ""
                port = int(port.group(1)) if port else 0
                if (host, port) in RESOLVER:
                    trace.refuse("resolves a name through the system resolver: the network")
                elif host and not (host.startswith("127.") or host in ("::1", "0.0.0.0", "::")
                                   or host.startswith("::ffff:127.")):
                    trace.refuse(f"connects to {host}:{port}: the network")
            elif family == "AF_UNIX":
                match = re.search(r'sun_path=("(?:[^"\\]|\\.)*")', address)
                path = unquote(match.group(1)) if match else None
                if path and path.startswith("/") and context.classify(path)[0] != "scratch":
                    trace.refuse(f"connects to the socket {path}: a host service")
            continue
        if name in ("openat", "open", "openat2", "creat"):
            if name == "creat":
                directory, path_arg, flags = None, arguments[0], "O_CREAT|O_WRONLY|O_TRUNC"
            elif name == "open":
                directory, path_arg, flags = None, arguments[0], arguments[1] if len(arguments) > 1 else ""
            else:
                directory, path_arg = decoration(arguments[0]), arguments[1]
                flags = arguments[2] if len(arguments) > 2 else ""
            path = resolve(pid, directory, unquote(path_arg))
            if any(flag in flags for flag in WRITE_FLAGS):
                if not failed:
                    write(pid, returned or path)
                if "O_CREAT" in flags or failed:
                    continue
            if failed:
                if errno in ("ENOENT", "ENOTDIR"):
                    read(pid, path, "absent")
                elif errno not in ("EEXIST", "EACCES", "ELOOP", "EISDIR", "ENXIO"):
                    trace = trace_of(pid)
                    if trace and context.classify(path or "/")[0] != "scratch":
                        trace.refuse(f"{name} of {path} failed with {errno}")
                continue
            read(pid, returned or path, "dir" if "O_DIRECTORY" in flags else "any")
            continue
        if name in WRITES:
            if name in ("renameat", "renameat2", "linkat"):
                pairs = [(decoration(arguments[0]), arguments[1]), (decoration(arguments[2]), arguments[3])]
            elif name in ("rename", "link"):
                pairs = [(None, arguments[0]), (None, arguments[1])]
            elif name == "symlink":
                pairs = [(None, arguments[1])]  # the first argument is the link's content
            elif name == "symlinkat":
                pairs = [(decoration(arguments[1]), arguments[2])]
            elif name in ("unlinkat", "mkdirat", "fchmodat", "fchownat", "utimensat", "mknodat"):
                pairs = [(decoration(arguments[0]), arguments[1])]
            else:
                pairs = [(None, arguments[0])]
            if not failed:
                for directory, argument in pairs:
                    write(pid, resolve(pid, directory, unquote(argument)))
            continue
        if name in STATS:
            at = name in ("newfstatat", "fstatat64", "statx", "faccessat", "faccessat2", "readlinkat",
                          "name_to_handle_at")
            directory = decoration(arguments[0]) if at else None
            path_text = unquote(arguments[1] if at else arguments[0]) if arguments else None
            if path_text is None or (at and path_text == "" and directory):
                continue  # a descriptor, already observed when opened
            path = resolve(pid, directory, path_text)
            if failed:
                if errno in ("ENOENT", "ENOTDIR"):
                    read(pid, path, "absent")
                continue
            mode = re.search(r"stx?_mode=S_IF([A-Z]+)", raw_arguments)
            read(pid, path, ("dir" if mode.group(1) == "DIR" else "file") if mode else "any")
            continue

    for pid in derived_pids:
        key = owner.get(pid) or start.get(pid, (None, None))[0]
        if key and derived_tree:
            tests.setdefault(key, TestTrace(*key)).query(f"tree:{derived_tree}", "any")
    return tests, written


def trace_directory(directory, drop):
    """A `records` function over a `strace -ff -o <directory>/t` trace: every
    process's lines except those `drop` (an extended regular expression) matches,
    filtered by grep, since the full trace runs to tens of gigabytes."""
    def records():
        command = ["grep", "-r", "-H", "-v", "-E", drop, str(directory)] if drop else \
            ["grep", "-r", "-H", "", str(directory)]
        with subprocess.Popen(command, stdout=subprocess.PIPE, env={**os.environ, "LC_ALL": "C"}) as process:
            for raw in process.stdout:
                name, _, text = raw.decode("utf-8", "surrogateescape").rstrip("\n").partition(":")
                pid = name.rsplit(".", 1)[-1]
                if pid.isdigit():
                    yield int(pid), text
    return records


def pids_matching(directory, pattern):
    """Processes whose trace mentions `pattern` (a fixed string)."""
    found = subprocess.run(["grep", "-r", "-l", "-F", pattern, str(directory)], stdout=subprocess.PIPE,
                           env={**os.environ, "LC_ALL": "C"}).stdout.decode().split()
    return {int(path.rsplit(".", 1)[-1]) for path in found if path.rsplit(".", 1)[-1].isdigit()}


def scratch_drop(prefixes):
    """The prefilter: path-only syscalls on scratch or derived paths, and calls the parser ignores."""
    paths = "|".join(re.escape(prefix) for prefix in prefixes)
    calls = "openat|open|stat|lstat|newfstatat|statx|access|faccessat|faccessat2|readlink|readlinkat|" \
            "getxattr|lgetxattr|statfs|unlink|unlinkat|mkdir|mkdirat|rmdir|utimensat|chmod|fchmodat"
    return rf"^[^:]*:(getcwd|exit|exit_group|wait4)\(|^[^:]*:({calls})\([^\"]*\"({paths})"


def answer_kind(answer):
    if answer == "absent":
        return "absent"
    if isinstance(answer, dict):
        return "dir" if "dir" in answer else "file"
    return "unreadable"


def checkout_writes(written, answers):
    """Split the run's checkout writes: `transient` paths were created and are
    gone again (scratch a test made in its working directory); `changed` are
    paths whose content or listing differs from the commit's during the run:
    every persistent write and the directory holding any write."""
    transient, changed = set(), set()
    root = getattr(answers, "root", None)
    for path in written:
        if answers(path) == "absent" and not (root and (Path(root) / path[3:]).exists()):
            transient.add(path)
        else:
            changed.add(path)
        parent = os.path.dirname(path[3:])
        changed.add("ws:" + parent)
    return transient, changed - transient


def trace_answers(trace, answers, transient=frozenset()):
    """The recorded trace: each query with its current answer, or problems when
    an answer is not what the test observed."""
    recorded, problems = [], list(trace.problems)
    for path in sorted(trace.queries):
        if path in transient or any(path.startswith(scratch + "/") for scratch in transient):
            continue
        answer = answers(path)
        observed, now = trace.queries[path], answer_kind(answer)
        if now == "unreadable":
            problems.append(f"{path} cannot be read to record it")
        elif observed not in ("any", now):
            problems.append(f"{path} was {observed} during the test and is {now} now")
        recorded.append([path, answer])
    return recorded, problems


# ---- the store ----


def store_root():
    return Path(os.environ.get("GATE_TEST_CACHE_DIR") or Path.home() / ".cache" / "gents-test-results") / f"v{SCHEMA}"


def entry_dir(root, test, key):
    name = hashlib.sha256(f"{test[0]}\0{test[1]}\0{key}".encode()).hexdigest()
    return root / name[:2] / name


def read_entry(root, test, key):
    """The records and contradiction for one test and key, as the model's store."""
    directory = entry_dir(root, test, key)
    store = {"records": [], "contradicted": []}
    if (directory / "contradicted.json").exists():
        store["contradicted"].append([list(test), key])
    try:
        names = sorted(os.listdir(directory))
    except FileNotFoundError:
        return store
    cutoff = time.time() - RETENTION_DAYS * 86400
    for name in names:
        if not name.startswith("pass-") or not name.endswith(".json"):
            continue
        path = directory / name
        try:
            if path.stat().st_mtime < cutoff:
                path.unlink(missing_ok=True)
                continue
            record = json.loads(path.read_text())
        except (OSError, ValueError):
            continue  # evicted between listing and reading, or not ours
        if record.get("schema") == SCHEMA:
            store["records"].append({"test": list(record["test"]), "key": record["key"],
                                     "trace": record["trace"], "run": record["run"]["id"],
                                     "provenance": record["run"]})
    return store


def publish(directory, name, document):
    directory.mkdir(parents=True, exist_ok=True)
    temporary = directory / f".{name}.{os.getpid()}.tmp"
    temporary.write_text(json.dumps(document, sort_keys=True))
    os.replace(temporary, directory / name)


# ---- what the gate calls ----


def run_identity():
    return {
        "id": "-".join(filter(None, [os.environ.get("GITHUB_RUN_ID", "local"),
                                     os.environ.get("GITHUB_RUN_ATTEMPT")])),
        "sha": subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, stdout=subprocess.PIPE,
                              text=True).stdout.strip(),
        "event": os.environ.get("GITHUB_EVENT_NAME", "local"),
        "ref": os.environ.get("GITHUB_REF", ""),
        "host": socket.gethostname().split(".")[0],
        "runner": os.environ.get("RUNNER_NAME", ""),
    }


def runner_inputs(policy, nextest_version):
    profile = ROOT / ".config" / "nextest.toml"
    return {
        "nextest": nextest_version,
        "nextest_config": sha256_file(profile) if profile.exists() else "",
        "environment": keyed_environment(policy, os.environ),
    }


def model_policy(policy, full):
    return {"full": full, "excluded": [list(test) for test in policy["excluded"]]}


def plan_run(listing, binaries, answers, mode, root=None):
    """Decide every test of an archive listing. Returns the cache plan the
    slices run and the per-test decisions for the report."""
    policy = load_policy()
    runner = runner_inputs(policy, binaries["nextest"])
    root = root or store_root()
    keys, decisions = {}, {}
    for binary_id, info in sorted(binaries["binaries"].items()):
        keys[binary_id] = static_key(policy, binary_id, answers("target:" + info["relative"]), runner)
    for binary_id, names in sorted(listing.items()):
        for name in names:
            test = [binary_id, name]
            exclusion = excluded(policy, binary_id, name)
            if exclusion:
                decisions[f"{binary_id} {name}"] = {"run": "excluded", "issue": exclusion["issue"]}
                continue
            store = read_entry(root, test, keys[binary_id]) if mode == "on" else {"records": [], "contradicted": []}
            outcome, detail = decide({"full": mode != "on", "excluded": []}, store, test, keys[binary_id], answers)
            if outcome == "skip":
                decisions[f"{binary_id} {name}"] = {"skip": detail["provenance"]}
            else:
                entry = {"run": detail}
                if detail == "miss" and store["records"]:
                    entry["changed"] = first_difference(store["records"], answers)
                decisions[f"{binary_id} {name}"] = entry
    return {"mode": mode, "keys": keys, "runner": runner, "decisions": decisions}


def first_difference(records, answers):
    """The first input that differs from the newest record's."""
    for record in sorted(records, key=lambda record: record.get("provenance", {}).get("id", ""), reverse=True)[:1]:
        for path, expected in record["trace"]:
            if answers(path) != expected:
                return path
    return None


def filters(plan, listing):
    """nextest -E arguments selecting exactly the tests the plan runs."""
    expressions = []
    for binary_id, names in sorted(listing.items()):
        run = [name for name in names if "run" in plan["decisions"][f"{binary_id} {name}"]]
        if not run:
            continue
        if len(run) == len(names):
            expressions.append(f"binary_id(={binary_id})")
            continue
        skipped = [name for name in names if name not in run]
        terms, inverted = (skipped, True) if len(skipped) < len(run) else (run, False)
        body = " | ".join(f"test(={name})" for name in terms)
        expressions.append(f"binary_id(={binary_id}) & {'not ' if inverted else ''}({body})")
    return [argument for expression in expressions for argument in ("-E", expression)] or ["-E", "none()"]


def junit_outcomes(path):
    """(binary id, test name) -> passed, from nextest's JUnit report."""
    outcomes = {}
    if not Path(path).exists():
        return outcomes
    for case in ElementTree.parse(path).getroot().iter("testcase"):
        failed = any(child.tag in ("failure", "error", "flakyFailure", "rerunFailure") for child in case)
        skipped = any(child.tag == "skipped" for child in case)
        if not skipped:
            outcomes[(case.get("classname"), case.get("name"))] = not failed
    return outcomes


def record_run(plan, outcomes, traces, written, answers, binary_ids, trusted, root=None):
    """Record a slice's passes and contradictions. Returns per-test notes for the
    report and the checkout paths the run changed.

    A test's checkout is clean (TestCache.lean `checkoutClean`) when it changed no
    checkout path and read none that any process of the run changed."""
    policy = load_policy()
    root = root or store_root()
    identity = run_identity()
    notes = {}
    transient, changed = checkout_writes(written, answers)
    for (binary_id, name), passed in sorted(outcomes.items()):
        label = f"{binary_id} {name}"
        key = plan["keys"].get(binary_id)
        if key is None:
            continue
        trace = traces.get((binary_ids.get(binary_id), name))
        if trace is None:
            recorded, problems, clean = [], ["the tracer saw no process for this test"], True
        else:
            recorded, problems = trace_answers(trace, answers, transient)
            clean = not (trace.wrote - transient) and not (set(trace.queries) & changed)
        execution = {
            "test": [binary_id, name], "key": key, "fs": answers, "passed": passed, "trace": recorded,
            "trusted": trusted, "traceComplete": not problems, "checkoutClean": clean, "run": identity["id"],
        }
        model = {"full": plan["mode"] != "on", "excluded": [[binary_id, name]] if excluded(policy, binary_id, name) else []}
        directory = entry_dir(root, [binary_id, name], key)
        if passed and vouched(model, execution):
            publish(directory, f"pass-{identity['id']}-{os.environ.get('GATE_SLICE', '0')}.json", {
                "schema": SCHEMA, "test": [binary_id, name], "key": key, "trace": recorded,
                "run": identity, "components": {"binary": binary_id, **plan["runner"]},
                "recorded": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            })
            notes[label] = "recorded"
            continue
        reasons = [reason for reason, holds in (
            ("failed", not passed), ("untrusted run", not trusted), ("checkout written during the run", not clean),
            ("excluded", bool(model["excluded"]))) if holds] + problems[:3]
        if not passed and vouched(model, execution):
            store = read_entry(root, [binary_id, name], key)
            if any(contradicts(model, execution, record) for record in store["records"]):
                publish(directory, "contradicted.json", {"schema": SCHEMA, "test": [binary_id, name], "key": key,
                                                         "run": identity})
                print(f"::error::test cache premise violated: {label} failed where a recorded pass vouched for "
                      f"the same inputs. Exclude it in .config/test-cache.toml with an issue.", flush=True)
                reasons.insert(0, "CONTRADICTED a recorded pass")
        notes[label] = "not recorded: " + "; ".join(reasons)
    return notes, sorted(changed)


def summarize(plan, notes=None, dirty=()):
    decisions = plan["decisions"]
    counts = {}
    for decision in decisions.values():
        reason = "skipped (passed before with these inputs)" if "skip" in decision else f"run: {decision['run']}"
        counts[reason] = counts.get(reason, 0) + 1
    lines = ["| tests | count |", "|---|---|"] + [f"| {reason} | {count} |" for reason, count in sorted(counts.items())]
    changed = {}
    for decision in decisions.values():
        if decision.get("changed"):
            changed[decision["changed"]] = changed.get(decision["changed"], 0) + 1
    if changed:
        lines += ["", "Inputs that changed since a recorded pass (tests):", ""]
        lines += [f"- `{path}` ({count})" for path, count in sorted(changed.items(), key=lambda item: -item[1])[:20]]
    if notes is not None:
        recorded = sum(1 for note in notes.values() if note == "recorded")
        reasons = {}
        for note in notes.values():
            if note != "recorded":
                for reason in note.removeprefix("not recorded: ").split("; "):
                    reasons[reason] = reasons.get(reason, 0) + 1
        lines += ["", f"Recorded {recorded} of {len(notes)} executed tests.", ""]
        lines += [f"- not recorded, {reason}: {count}" for reason, count in
                  sorted(reasons.items(), key=lambda item: -item[1])[:20]]
        if dirty:
            lines += ["", "Checkout paths written during the run: " + ", ".join(f"`{path}`" for path in list(dirty)[:10])]
    return "\n".join(lines)


def tracer():
    """The strace command prefix, or None where it cannot run."""
    if sys.platform != "linux" or not shutil.which("strace"):
        return None
    return ["strace", "-ff", "-qq", "--seccomp-bpf", "-y", "-s", "4096", "-e", f"trace={TRACE_SYSCALLS}",
            "-e", "signal=none"]

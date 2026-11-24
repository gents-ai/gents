"""Host side of scripts/evals/run-container.sh, run on the docker host.

    orchestrate.py run RUN_DIR      plan the slots, run every slot not done, summarize
    orchestrate.py status RUN_DIR   slot states and the summary so far

A slot is one trial of one case, and each attempt at it is one `--network none`
container. The host writes the slot's spec and target into `control/`, which
the container mounts read-only at /slot; the trial writes only to /eval, a
size-limited tmpfs, and /tmp. PID 1 is an inert `sleep` that outlives the
attempt's wall-clock limit by EXPORT_MARGIN_SECS, so an orphaned container
ends by itself. The entrypoint runs through `docker exec`; when it ends or its
limit passes, every process in the container is killed and /eval is exported
as a tar the host extracts under its own directory, regular files and
directories only, within size and count limits. Nothing the trial wrote is
ever written through, followed or used as a host path: it is read with
O_NOFOLLOW from that export and validated. The model is reached through a
relay socket opened for the attempt alone, on a replica checked just before.
A slot is done once an attempt exited 0 with a valid result; `run` again
continues every other slot.
"""

import argparse
import asyncio
import datetime
import errno
import hashlib
import json
import os
import pathlib
import queue
import re
import resource
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import threading
import urllib.parse
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent)]
import relay  # noqa: E402
import suites as registry  # noqa: E402

MAX_ATTEMPTS = 3
NOTHING_RAN = 3
EXPORT_MARGIN_SECS = 900
EXPORT_FILES = 200_000
RESULT_BYTES = 64 * 1024
REPORT_BYTES = 32 * 1024 * 1024
HEALTH_RETRY_SECS = 30
HEALTH_CHECKS = 10
ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
IMAGE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/:@-]{0,254}$")
SPLITS = {"train", "validation", "held_out"}
COMMAND_EXECUTION = {"off", "unrestricted"}
FIXTURES = str(registry.FIXTURES)
ENGINEER_SUBJECT = "ladder/engineer_subject"


class Untrusted(Exception):
    """A file a trial wrote is missing, unsafe or malformed."""


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def load(path):
    return json.loads(pathlib.Path(path).read_text())


def dump(path, value):
    path = pathlib.Path(path)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    os.replace(temporary, path)


def append(path, value):
    with open(path, "a", encoding="utf-8") as out:
        out.write(json.dumps(value) + "\n")


def log(message):
    print(f"[{now()}] {message}", flush=True)


def checked(kind, value, pattern=ID):
    if not isinstance(value, str) or not pattern.match(value):
        raise ValueError(f"{kind} {value!r} must match {pattern.pattern}")
    return value


def size_bytes(text):
    match = re.fullmatch(r"([0-9]+)([kmgt]?)", str(text).strip().lower())
    if not match:
        raise ValueError(f"{text!r} is not a size like 512m or 2g")
    return int(match.group(1)) * 1024 ** " kmgt".index(match.group(2) or " ")


def mount_path(path):
    text = str(path)
    if "," in text or "=" in text or "\n" in text:
        raise ValueError(f"{text} cannot be a docker mount source")
    return text


def read_untrusted(root, parts, limit):
    """Read root/parts... without following a link at any step: only a
    regular file of at most `limit` bytes."""
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC
    if not parts or any(not isinstance(part, str) or not ID.match(part) for part in parts):
        raise Untrusted(f"{parts!r} is not a plain relative path")
    try:
        directory = os.open(root, flags | os.O_DIRECTORY)
    except OSError as error:
        raise Untrusted(f"{root}: {error.strerror}") from None
    try:
        for part in parts[:-1]:
            try:
                child = os.open(part, flags | os.O_DIRECTORY, dir_fd=directory)
            except OSError as error:
                raise Untrusted(f"{'/'.join(parts)}: {error.strerror}") from None
            os.close(directory)
            directory = child
        try:
            descriptor = os.open(parts[-1], flags | os.O_NONBLOCK | os.O_NOCTTY, dir_fd=directory)
        except OSError as error:
            reason = "a link" if error.errno in (errno.ELOOP, errno.EMLINK) else error.strerror
            raise Untrusted(f"{'/'.join(parts)}: {reason}") from None
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            os.close(descriptor)
            raise Untrusted(f"{'/'.join(parts)} is not a regular file")
        with os.fdopen(descriptor, "rb") as handle:
            if info.st_size > limit:
                raise Untrusted(f"{'/'.join(parts)} is {info.st_size} bytes, over {limit}")
            data = handle.read(limit + 1)
        if len(data) > limit:
            raise Untrusted(f"{'/'.join(parts)} grew past {limit} bytes")
        return data
    finally:
        os.close(directory)


def untrusted_json(root, parts, limit):
    try:
        return json.loads(read_untrusted(root, parts, limit))
    except (ValueError, UnicodeDecodeError) as error:
        raise Untrusted(f"{'/'.join(parts)} is not JSON: {error}") from None


def validated_result(value):
    if not isinstance(value, dict):
        raise Untrusted("result.json is not an object")
    exit_code, run_ids = value.get("exit_code"), value.get("run_ids")
    if not isinstance(exit_code, int) or isinstance(exit_code, bool) or not 0 <= exit_code <= 255:
        raise Untrusted("result.json exit_code is not 0..255")
    if not isinstance(run_ids, list) or len(run_ids) > 1:
        raise Untrusted("result.json run_ids is not a list of at most one run")
    for run_id in run_ids:
        try:
            checked("run id", run_id)
        except ValueError as error:
            raise Untrusted(str(error)) from None
    return {"exit_code": exit_code, "run_id": run_ids[0] if run_ids else None}


def count(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def validated_report(report, case_id):
    try:
        cell = report["report"]["cells"][0]
        counts = {key: count(cell["counts"].get(key)) for key in ("pass", "fail", "unknown", "not_evidence", "abandoned")}
        usage = {key: count(cell["usage"].get(key)) for key in ("input_tokens", "output_tokens")}
        trial = next((s for s in cell["slots"] if isinstance(s, dict) and s.get("case_id") == case_id), None)
    except (KeyError, IndexError, TypeError, AttributeError):
        raise Untrusted("report.json does not have a cell with counts, usage and slots") from None
    entry = {"counts": counts, **usage}
    if trial is not None:
        klass = trial.get("class")
        entry["class"] = klass if klass in ("pass", "fail", "unknown", "not_evidence", "abandoned", "planned") else None
        entry["score_bp"] = count(trial.get("score_bp"))
        entry["verdicts"] = [
            {key: relay.clipped(str(verdict.get(key))) for key in ("stage_id", "check", "kind", "reason_code", "detail")}
            for verdict in trial.get("verdicts", [])[:64]
            if isinstance(verdict, dict)
        ]
    return entry


class BoundedReader:
    def __init__(self, stream, limit):
        self.stream, self.limit, self.read_bytes = stream, limit, 0

    def read(self, size=-1):
        data = self.stream.read(size if size and size > 0 else relay.CHUNK)
        self.read_bytes += len(data)
        if self.read_bytes > self.limit:
            raise Untrusted(f"the export passed {self.limit} bytes")
        return data


def extract_export(stream, destination, limit_bytes, limit_files=EXPORT_FILES):
    """Extract a tar stream of a trial's /eval: regular files and directories
    only, relative and inside `destination`; returns what was skipped."""
    skipped = []
    files = 0

    def members(archive):
        nonlocal files
        for member in archive:
            files += 1
            if files > limit_files:
                raise Untrusted(f"the export passed {limit_files} entries")
            name = member.name
            if not (member.isreg() or member.isdir()):
                skipped.append(f"{relay.clipped(name)} (not a regular file or directory)")
                continue
            parts = pathlib.PurePosixPath(name).parts
            if name.startswith("/") or ".." in parts or any("\0" in part for part in parts):
                skipped.append(f"{relay.clipped(name)} (outside the export)")
                continue
            yield member

    destination.mkdir(parents=True, exist_ok=False)
    reader = BoundedReader(stream, limit_bytes)
    with tarfile.open(fileobj=reader, mode="r|") as archive:
        archive.extractall(destination, members=members(archive), filter="data")
    return skipped


class Replica:
    def __init__(self, run_dir, name):
        self.name = checked("replica", name)
        self.target = run_dir / "targets" / f"{name}.json"
        target = load(self.target)
        endpoint = target["inference_backends"][0]["endpoint"]
        self.model = target["inference_profiles"][0]["model_name"]
        url = urllib.parse.urlparse(endpoint)
        if url.scheme != "http" or url.hostname != "127.0.0.1" or not url.port:
            raise SystemExit(f"{self.target}: the endpoint must be http://127.0.0.1:<port>/..., not {endpoint}")
        self.port = url.port

    def healthy(self):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/v1/models", timeout=10) as response:
                return response.status == 200
        except OSError:
            return False


class Relays:
    """The per-attempt filtering relays, on one event loop thread."""

    def __init__(self):
        self.loop = asyncio.new_event_loop()
        threading.Thread(target=self.loop.run_forever, daemon=True).start()
        self.open = {}

    def call(self, coroutine):
        return asyncio.run_coroutine_threadsafe(coroutine, self.loop).result()

    def start(self, socket_path, replica, log_path):
        self.open[socket_path] = self.call(relay.listen(str(socket_path), replica.port, str(log_path),
                                                        model=replica.model))

    def stop(self, socket_path):
        server, relay_log = self.open.pop(socket_path)

        async def shut():
            server.close()
            await asyncio.wait_for(server.wait_closed(), 30)

        try:
            self.call(shut())
        except asyncio.TimeoutError:
            log(f"relay {socket_path} kept connections past its close")
        relay_log.close()
        socket_path.unlink(missing_ok=True)


class Run:
    def __init__(self, run_dir):
        self.dir = pathlib.Path(run_dir).resolve()
        mount_path(self.dir)
        self.config = load(self.dir / "run.json")
        self.name = checked("run name", self.config["name"])
        checked("image", self.config["image"], IMAGE)
        for suite in self.config["suites"]:
            checked("suite", suite)
        if self.config["command_execution"] not in COMMAND_EXECUTION:
            raise ValueError(f"command_execution must be one of {sorted(COMMAND_EXECUTION)}")
        size_bytes(self.config["limits"]["disk"])
        self.replicas = [Replica(self.dir, name) for name in self.config["replicas"]]
        self.stopping = threading.Event()
        self.running = {}
        self.tries = {}
        self.lock = threading.Lock()
        workers = len(self.replicas) * self.config["per_replica"]
        self.admission = threading.BoundedSemaphore(self.config.get("max_running") or workers)

    def docker(self, *args, **kwargs):
        return subprocess.run(["docker", *args], check=True, text=True, capture_output=True, **kwargs).stdout

    def source(self, *parts):
        return self.dir.joinpath("source", *parts)

    def overrides(self):
        directory = self.dir / "overrides"
        if directory.is_dir() and any(directory.iterdir()):
            return ["--mount", f"type=bind,src={mount_path(directory)},dst=/overrides,readonly"]
        return []

    def definition_dir(self, suite):
        override = self.dir / "overrides" / "definition"
        return override if override.is_dir() else self.source(FIXTURES, suite["path"])

    def suite_view(self, suite):
        """The fixtures one suite's trials see: its definition and subjects only."""
        view = self.dir / "suites" / suite["id"]
        if not view.exists():
            staging = view.with_name(view.name + ".tmp")
            shutil.rmtree(staging, ignore_errors=True)
            wanted = {ENGINEER_SUBJECT, suite["subject"], suite["path"]}
            for relative in sorted(wanted):
                shutil.copytree(self.source(FIXTURES, relative), staging / relative, symlinks=True)
            os.replace(staging, view)
        return view

    def plan(self):
        matrix = registry.matrix(self.source())
        selected = [s for selection in self.config["suites"]
                    for s in (matrix["default"] if selection == "all" else [selection])]
        suites = {}
        slots = []
        for suite_id in selected:
            suite = next((s for s in matrix["suites"] if s["id"] == suite_id), None)
            if suite is None:
                raise SystemExit(f"unknown suite {suite_id!r}; see scripts/evals/run-ladder.sh list")
            checked("suite", suite_id)
            suites[suite_id] = {**suite, "view": self.suite_view(suite)}
            root = self.definition_dir(suite)
            definition = load(root / "pack_config.json")["eval_definitions"][0]
            for relative in definition["cases"]:
                case = load(root / relative)
                case_id = checked(f"case of suite {suite_id}", case["case_id"])
                if case["split"] not in SPLITS:
                    raise SystemExit(f"case {case_id} has unknown split {case['split']!r}")
                if case["split"] not in self.config["splits"]:
                    continue
                for trial_index in range(self.config["trials"]):
                    slot_id = checked("slot", f"{suite_id}.{case_id}.t{trial_index}")
                    control = self.dir / "slots" / slot_id / "control"
                    if not (control / "slot.json").exists():
                        control.mkdir(parents=True, exist_ok=True)
                        dump(control / "slot.json", {
                            "suite": suite_id, "case_id": case_id, "split": case["split"],
                            "trial_index": trial_index,
                            "seed_base": self.config["seed_base"] + trial_index,
                        })
                    slots.append(slot_id)
        if not slots:
            raise SystemExit("the selected suites and splits contain no cases")
        return slots, suites

    def state(self, slot_id):
        path = self.dir / "slots" / slot_id / "state.json"
        return load(path) if path.exists() else {}

    def done(self, slot_id):
        return self.state(slot_id).get("state") == "done"

    def attempts(self, slot_id):
        ledger = self.dir / "logs" / slot_id / "attempts.jsonl"
        return sum(1 for line in ledger.read_text().splitlines() if '"started"' in line) if ledger.exists() else 0

    def digest(self, slot_id):
        return hashlib.sha256(f"{self.name}/{slot_id}".encode()).hexdigest()[:12]

    def container(self, slot_id):
        return f"gents-eval-{self.digest(slot_id)}"

    def kill(self, name):
        subprocess.run(["docker", "kill", name], capture_output=True)

    def register(self, slot_id, name):
        with self.lock:
            if self.stopping.is_set():
                return False
            self.running[slot_id] = name
            return True

    def run_slot(self, slot_id, replica, relays, suite):
        slot = self.dir / "slots" / slot_id
        control = slot / "control"
        logs = self.dir / "logs" / slot_id
        sockets = self.dir / "relay" / self.digest(slot_id)
        logs.mkdir(parents=True, exist_ok=True)
        sockets.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(replica.target, control / "target.json.tmp")
        os.replace(control / "target.json.tmp", control / "target.json")
        name = self.container(slot_id)
        stale = self.docker("ps", "-aq", "--filter", f"name=^/{name}$", "--filter", f"label=gents-eval-run={self.name}")
        if stale.strip():
            self.docker("rm", "-f", name)
        attempt = self.attempts(slot_id) + 1
        socket_path = sockets / "model.sock"
        if len(str(socket_path)) > 100:
            raise ValueError(f"{socket_path} is too long for a unix socket; use a shorter run name or root")
        limits = self.config["limits"]
        timeout = int(limits["trial_secs"])
        environment = {"HOME": "/eval/user", "NVIDIA_VISIBLE_DEVICES": "void", "GENTS_EVAL_TARGET": replica.name}
        environment["GENTS_EVAL_COMMAND_EXECUTION"] = self.config["command_execution"]
        if self.config["command_execution"] == "unrestricted":
            environment["GENTS_EVAL_ALLOW_HOST_BASH"] = "1"
        uid, gid = os.getuid(), os.getgid()
        start = [
            "docker", "run", "-d", "--rm", "--name", name,
            "--label", f"gents-eval-run={self.name}", "--label", f"gents-eval-slot={slot_id}",
            "--runtime", "runc", "--network", "none", "--hostname", "trial", "--user", f"{uid}:{gid}",
            "--read-only",
            "--tmpfs", f"/eval:rw,exec,nosuid,nodev,size={limits['disk']},uid={uid},gid={gid},mode=0700",
            "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=1g,mode=1777",
            "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            "--cpus", str(limits["cpus"]), "--memory", limits["memory"], "--memory-swap", limits["memory"],
            "--pids-limit", str(limits["pids"]), "--ulimit", "nofile=65536:65536",
            *[part for key, value in environment.items() for part in ("--env", f"{key}={value}")],
            "--mount", f"type=bind,src={mount_path(control)},dst=/slot,readonly",
            "--mount", f"type=bind,src={mount_path(sockets)},dst=/relay,readonly",
            "--mount", f"type=bind,src={mount_path(self.dir / 'launcher')},dst=/launcher,readonly",
            "--mount", f"type=bind,src={mount_path(suite['view'])},dst=/opt/gents/src/{FIXTURES},readonly",
            *self.overrides(),
            self.config["image"], "sleep", str(timeout + EXPORT_MARGIN_SECS),
        ]
        if not self.register(slot_id, name):
            return
        append(logs / "attempts.jsonl", {"event": "started", "attempt": attempt, "replica": replica.name,
                                         "container": name, "at": now()})
        log(f"{slot_id}: attempt {attempt} on {replica.name} ({name})")
        relays.start(socket_path, replica, logs / "relay.jsonl")
        status, outcome, export = None, "exited", None
        try:
            subprocess.run(start, check=True, capture_output=True)
            if self.stopping.is_set():
                self.kill(name)
            with open(logs / f"attempt-{attempt}.log", "wb") as output:
                try:
                    status = subprocess.run(["docker", "exec", name, "bash", "/launcher/entrypoint.sh"],
                                            stdout=output, stderr=subprocess.STDOUT, timeout=timeout).returncode
                except subprocess.TimeoutExpired:
                    outcome = "timed out"
                    log(f"{slot_id}: attempt {attempt} passed its {timeout}s limit")
            if self.stopping.is_set():
                outcome = "stopped"
            subprocess.run(["docker", "exec", name, "bash", "-c",
                            "for _ in 1 2 3; do kill -KILL -1 2>/dev/null; sleep 0.2; done"],
                           capture_output=True, timeout=60)
            export = self.export(name, slot / "attempts" / str(attempt), logs, limits)
        finally:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True)
            with self.lock:
                self.running.pop(slot_id, None)
            relays.stop(socket_path)
        self.settle(slot_id, attempt, status, outcome, export)

    def export(self, name, destination, logs, limits):
        shutil.rmtree(destination, ignore_errors=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        process = subprocess.Popen(["docker", "exec", name, "tar", "-C", "/eval", "--numeric-owner", "-cf", "-", "."],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            skipped = extract_export(process.stdout, destination / "eval", size_bytes(limits["disk"]) + 64 * 1024 * 1024)
            note = {"skipped": skipped[:100], "skipped_count": len(skipped)}
        except (Untrusted, tarfile.TarError, OSError) as error:
            note = {"error": relay.clipped(str(error))}
        finally:
            process.kill()
            process.wait()
        append(logs / "exports.jsonl", {"at": now(), "destination": str(destination), **note})
        return destination / "eval"

    def settle(self, slot_id, attempt, status, outcome, export):
        state = {"attempt": attempt, "exit_code": status, "outcome": outcome, "export": str(export) if export else None}
        try:
            if export is None:
                raise Untrusted("the attempt left no export")
            result = validated_result(untrusted_json(export, ["result.json"], RESULT_BYTES))
            state["run_id"] = result["run_id"]
            if status == 0 and result["exit_code"] == 0 and result["run_id"]:
                state["state"] = "done"
            elif status == NOTHING_RAN:
                state["state"] = "refused"
            else:
                state["state"] = outcome if outcome != "exited" else f"exit {status}"
        except Untrusted as error:
            state["state"] = outcome if outcome != "exited" else f"exit {status}"
            state["error"] = relay.clipped(str(error))
        dump(self.dir / "slots" / slot_id / "state.json", state)
        append(self.dir / "logs" / slot_id / "attempts.jsonl", {"event": "ended", "at": now(), **state})
        log(f"{slot_id}: attempt {attempt} {state['state']}")

    def replica_ready(self, replica):
        for check in range(HEALTH_CHECKS):
            if replica.healthy():
                return True
            log(f"replica {replica.name} is not serving /v1/models (check {check + 1} of {HEALTH_CHECKS})")
            if self.stopping.wait(HEALTH_RETRY_SECS):
                return False
        return False

    def worker(self, replica, work, relays, suites):
        while not self.stopping.is_set():
            try:
                slot_id = work.get_nowait()
            except queue.Empty:
                return
            with self.admission:
                if self.stopping.is_set():
                    return
                if not self.replica_ready(replica):
                    work.put(slot_id)
                    log(f"replica {replica.name} takes no more slots this pass")
                    return
                with self.lock:
                    self.tries[slot_id] = self.tries.get(slot_id, 0) + 1
                    tries = self.tries[slot_id]
                try:
                    spec = load(self.dir / "slots" / slot_id / "control" / "slot.json")
                    self.run_slot(slot_id, replica, relays, suites[spec["suite"]])
                    state = self.state(slot_id).get("state")
                except Exception as error:  # noqa: BLE001 - a slot's failure must not stop its replica's other slots
                    log(f"{slot_id}: launcher error {error!r}")
                    state = None
            if state in ("done", "refused") or self.stopping.is_set():
                continue
            if tries < MAX_ATTEMPTS:
                work.put(slot_id)
            else:
                log(f"{slot_id}: {MAX_ATTEMPTS} attempts this pass did not finish it; see logs/{slot_id}/")

    def stop(self, *_):
        with self.lock:
            if self.stopping.is_set():
                return
            self.stopping.set()
            names = list(self.running.values())
        log("stopping: no new slots; killing this run's running containers")
        for name in names:
            self.kill(name)

    def held_by(self):
        pid_file = self.dir / "orchestrator.pid"
        try:
            pid = int(pid_file.read_text())
            command = pathlib.Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
        except (OSError, ValueError):
            return None
        if b"run" in command and str(self.dir).encode() in command and any(b"orchestrate.py" in c for c in command):
            return pid
        return None

    def execute(self):
        holder = self.held_by()
        if holder is not None:
            raise SystemExit(f"run {self.name} is already orchestrated by process {holder}")
        pid_file = self.dir / "orchestrator.pid"
        pid_file.write_text(str(os.getpid()))
        signal.signal(signal.SIGTERM, self.stop)
        signal.signal(signal.SIGINT, self.stop)
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(resource.RLIMIT_NOFILE, (min(hard, 65536) if hard != resource.RLIM_INFINITY else 65536, hard))
        try:
            slots, suites = self.plan()
            pending = [slot_id for slot_id in slots if not self.done(slot_id)]
            log(f"run {self.name}: {len(slots)} slots, {len(pending)} to run on "
                f"{', '.join(r.name for r in self.replicas)} x {self.config['per_replica']}, "
                f"at most {self.config.get('max_running') or 'one per worker'} at once")
            work = queue.Queue()
            for slot_id in pending:
                work.put(slot_id)
            relays = Relays()
            threads = [
                threading.Thread(target=self.worker, args=(replica, work, relays, suites))
                for replica in self.replicas
                for _ in range(self.config["per_replica"])
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
            summary = summarize(self, slots)
            log(f"summary: {json.dumps(summary['totals'])}")
            return 0 if summary["totals"]["unfinished"] == 0 else 1
        finally:
            pid_file.unlink(missing_ok=True)


def slot_summary(run, slot_id):
    spec = load(run.dir / "slots" / slot_id / "control" / "slot.json")
    state = run.state(slot_id)
    entry = {"slot": slot_id, **spec, "state": state.get("state", "pending"), "attempts": run.attempts(slot_id)}
    ledger = run.dir / "logs" / slot_id / "attempts.jsonl"
    if ledger.exists():
        entry["replicas"] = [json.loads(line).get("replica") for line in ledger.read_text().splitlines()
                             if '"started"' in line]
    for key in ("outcome", "error", "run_id"):
        if key in state:
            entry[key] = state[key]
    if entry["state"] != "done":
        return entry
    try:
        report = untrusted_json(pathlib.Path(state["export"]),
                                ["home", "eval", "runs", str(state["run_id"]), "report.json"], REPORT_BYTES)
        entry.update(validated_report(report, spec["case_id"]))
    except (Untrusted, ValueError) as error:
        entry["state"] = "unreadable"
        entry["error"] = relay.clipped(str(error))
    return entry


def summarize(run, slots):
    entries = []
    for slot_id in slots:
        try:
            entries.append(slot_summary(run, slot_id))
        except (OSError, ValueError, KeyError) as error:
            entries.append({"slot": slot_id, "state": "unreadable", "error": relay.clipped(str(error))})
    totals = {"slots": len(entries), "unfinished": sum(entry["state"] != "done" for entry in entries)}
    for key in ("pass", "fail", "unknown", "not_evidence", "abandoned"):
        totals[key] = sum(entry.get("class") == key for entry in entries)
    for key in ("input_tokens", "output_tokens"):
        totals[key] = sum(entry.get(key) or 0 for entry in entries)
    host_bash = run.config["command_execution"] == "unrestricted"
    summary = {
        "run": run.name, "image": run.config["image"], "image_commit": run.config.get("image_commit"),
        "command_execution": run.config["command_execution"],
        "results": "unauthenticated" if host_bash else "grader-written",
        "results_note": (
            "host bash ran as the uid of each trial's runner and grader, so the trial could rewrite its own "
            "result.json and report.json; treat these results as unauthenticated"
            if host_bash else
            "no host bash: the trial's file tools were confined to its workspace, so only its runner and "
            "grader wrote result.json and report.json"
        ),
        "written_at": now(), "totals": totals, "slots": entries,
    }
    dump(run.dir / "summary.json", summary)
    lines = [f"results: {summary['results']}",
             f"{'slot':<64} {'state':<10} {'class':<12} {'score':>6} {'in tok':>8} {'out tok':>8}"]
    for entry in entries:
        score = entry.get("score_bp")
        tokens = [entry.get(key) for key in ("input_tokens", "output_tokens")]
        lines.append(f"{entry['slot']:<64} {entry['state']:<10} {entry.get('class') or '-':<12} "
                     f"{'-' if score is None else score:>6} "
                     + " ".join(f"{'-' if count is None else count:>8}" for count in tokens))
    lines.append(json.dumps(totals))
    (run.dir / "summary.txt").write_text("\n".join(lines) + "\n")
    return summary


def status(run_dir):
    run = Run(run_dir)
    slots = sorted(path.name for path in (run.dir / "slots").iterdir() if (path / "control" / "slot.json").exists())
    running = run.docker("ps", "--filter", f"label=gents-eval-run={run.name}",
                         "--format", '{{.Label "gents-eval-slot"}}').split()
    summary = summarize(run, slots)
    print((run.dir / "summary.txt").read_text(), end="")
    holder = run.held_by()
    print(f"orchestrator: {'process ' + str(holder) if holder else 'not running'}; "
          f"running slots: {', '.join(running) or 'none'}")
    return 0 if summary["totals"]["unfinished"] == 0 else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["run", "status"])
    parser.add_argument("run_dir")
    args = parser.parse_args()
    try:
        if args.command == "run":
            return Run(args.run_dir).execute()
        return status(args.run_dir)
    except ValueError as error:
        log(f"refused: {error}")
        return 2


if __name__ == "__main__":
    sys.exit(main())

"""Authenticated loopback transport for Browser Use; Python runs as the operator."""

from __future__ import annotations

import argparse
import base64
import contextlib
import fcntl
import hashlib
import hmac
import importlib.metadata
import io
import json
import os
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.request import Request, urlopen

import browser_connection

MAX_OUTPUT = 2 * 1024 * 1024
STARTUP_TIMEOUT = 60
NAME = re.compile(r"[A-Za-z0-9_-]{1,64}\Z")


def failure(status, error, phase, code_executed, next_step, **details):
    return {
        "response": {
            "status": status,
            "error": str(error),
            "phase": phase,
            "code_executed": code_executed,
            "next": next_step,
            **details,
        }
    }


def write_json(path, value):
    path = Path(path)
    temp = path.with_suffix(".tmp")
    with open(temp, "w", encoding="utf-8") as out:
        os.chmod(temp, 0o600)
        json.dump(value, out)
    temp.replace(path)


def terminate(proc):
    if proc is not None:
        with contextlib.suppress(ProcessLookupError):
            if os.getpgid(proc.pid) == os.getpgrp():
                proc.kill()
            else:
                os.killpg(proc.pid, signal.SIGKILL)
        with contextlib.suppress(subprocess.TimeoutExpired):
            proc.wait(timeout=5)


def chrome_binary():
    for path in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        shutil.which("chromium"),
        shutil.which("chromium-browser"),
        shutil.which("google-chrome"),
        shutil.which("google-chrome-stable"),
    ]:
        if path and Path(path).is_file():
            return path
    raise RuntimeError(
        "Chromium unavailable. Install Chrome/Chromium or rerun setup with --cdp <endpoint>."
    )


class Session:
    def __init__(self, root, name, cdp=None, headed=False, live_browser=None):
        self.root = root / "sessions" / name
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.lock = threading.Lock()
        self.process = None
        self.browser = None
        self.last_used = time.monotonic()
        self.closed = False
        self.attached = bool(cdp)
        self.cdp = cdp
        self.headed = headed
        self.live_browser = live_browser
        self.connected = False
        self.selected_target = None

        self.runtime = tempfile.TemporaryDirectory(prefix="gb-", dir="/tmp")
        self.env = {
            key: os.environ[key]
            for key in ("PATH", "HOME", "USER", "LANG", "DISPLAY", "TMPDIR")
            if key in os.environ
        }

        paths = self.env.get("PATH", "").split(os.pathsep)
        self.env["PATH"] = os.pathsep.join(
            dict.fromkeys(
                paths + ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            )
        )
        self.workspace = self.root / "workspace"
        self.workspace.mkdir(exist_ok=True)
        self.env.update(
            BH_HOME=str(self.root / "harness"),
            BU_NAME="default"
            if live_browser
            else hashlib.sha256(name.encode()).hexdigest()[:12],
            BH_RUNTIME_DIR=self.runtime.name,
            BH_AGENT_WORKSPACE=str(self.root / "workspace"),
            ANONYMIZED_TELEMETRY="false",
            PYTHONUNBUFFERED="1",
            BH_TAB_MARKER="0" if live_browser else "1",
            GENTS_MANAGED_SERVICE=os.environ.get("GENTS_MANAGED_SERVICE", ""),
            GENTS_LIVE_BROWSER="1" if live_browser else "",
        )

        if cdp:
            self.env[
                "BU_CDP_URL" if cdp.startswith(("http://", "https://")) else "BU_CDP_WS"
            ] = cdp

    def start(self):
        if self.closed:
            raise RuntimeError("Session closed before execution")
        if self.cdp:
            if self.browser is not None and self.browser.poll() is not None:
                raise RuntimeError(
                    "Managed browser exited. Close and reopen this session explicitly; browser identity was not changed."
                )
            return
        try:
            profile = self.root / "profile"
            profile.mkdir(exist_ok=True)
            active = profile / "DevToolsActivePort"
            active.unlink(missing_ok=True)
            args = [
                chrome_binary(),
                f"--user-data-dir={profile}",
                "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
            ]
            if not self.headed:
                args += ["--headless=new"]
            with open(self.root / "chrome.log", "wb") as log:
                self.browser = subprocess.Popen(
                    args + ["about:blank"],
                    stdout=log,
                    stderr=log,
                    env=self.env,
                    start_new_session=not os.environ.get("GENTS_MANAGED_SERVICE"),
                )
            until = time.monotonic() + STARTUP_TIMEOUT
            while time.monotonic() < until:
                if self.closed:
                    raise RuntimeError("Session closed during browser startup")
                if self.browser.poll() is not None:
                    raise RuntimeError(
                        f"Chrome exited during startup; inspect {self.root / 'chrome.log'}"
                    )
                if active.exists():
                    try:
                        lines = active.read_text().splitlines()
                        if len(lines) >= 2:
                            endpoint = f"http://127.0.0.1:{int(lines[0])}"
                            with urlopen(
                                endpoint + "/json/version", timeout=1
                            ) as response:
                                if json.load(response).get("webSocketDebuggerUrl"):
                                    self.cdp = endpoint
                                    self.env["BU_CDP_URL"] = endpoint
                                    return
                    except (OSError, ValueError):
                        pass
                time.sleep(0.1)
            raise RuntimeError(
                f"Chrome startup timed out after {STARTUP_TIMEOUT}s; inspect {self.root / 'chrome.log'}. No browser code was executed. Check host process launch health before retrying this session."
            )
        except Exception:
            terminate(self.browser)
            self.browser = None
            raise

    def close(self):
        self.closed = True
        terminate(self.process)

        with contextlib.suppress(Exception):
            subprocess.run(
                [sys.executable, "-m", "browser_harness.run", "--reload"],
                check=False,
                env=self.env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=5,
            )
        terminate(self.browser)
        self.runtime.cleanup()

    def execute(self, code, timeout, job):
        try:
            self.start()
        except Exception as exc:
            return failure(
                "startup_failed",
                exc,
                "startup",
                False,
                "Check the Chrome log and host launch health. If the managed browser exited, close/reopen this session explicitly. This call's code did not run.",
                artifacts={"chrome_log": str(self.root / "chrome.log")},
            )
        run_dir = self.root / "runs" / job
        run_dir.mkdir(parents=True, mode=0o700)
        images_file = run_dir / "images.json"

        prelude = f"""import json as _json, pathlib as _pathlib
_image_paths = []
_capture = capture_screenshot
def capture_screenshot(path=None, full=False, max_dim=1568):
    path = path or str(_pathlib.Path({str(run_dir)!r}) / ("screen-" + str(len(_image_paths)) + ".png"))
    result = _capture(path=path, full=full, max_dim=max_dim)
    _image_paths.append(str(_pathlib.Path(path).resolve()))
    _pathlib.Path({str(images_file)!r}).write_text(_json.dumps(_image_paths[-2:]))
    return result
"""

        if self.attached and not self.live_browser:
            prelude += f"""from browser_harness import _ipc as _browser_ipc
import os as _browser_os
_driver_pid = _browser_ipc.pid_path(_browser_os.environ['BU_NAME']).read_text().strip()
_target_file = _pathlib.Path({self.runtime.name!r}) / ('target-' + _driver_pid + '.json')
if not _target_file.exists():
    _target = cdp("Target.createTarget", url="about:blank")["targetId"]
    switch_tab(_target)
    _target_file.write_text(_json.dumps(_target))
"""
        stdout_path = run_dir / "stdout.txt"
        stderr_path = run_dir / "stderr.txt"
        exceeded = threading.Event()

        def drain(pipe, path):
            size = 0
            with open(path, "wb", buffering=0) as out:
                while chunk := pipe.read(8192):
                    remaining = max(0, MAX_OUTPUT - size)
                    out.write(chunk[:remaining])
                    size += len(chunk)
                    if size > MAX_OUTPUT:
                        exceeded.set()
                        terminate(self.process)
                        break
            pipe.close()

        started = time.monotonic()
        self.process = subprocess.Popen(
            [sys.executable, str(Path(__file__).with_name("managed_run.py"))],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=self.workspace,
            env=self.env,
            start_new_session=not os.environ.get("GENTS_MANAGED_SERVICE"),
        )
        readers = [
            threading.Thread(
                target=drain, args=(self.process.stdout, stdout_path), daemon=True
            ),
            threading.Thread(
                target=drain, args=(self.process.stderr, stderr_path), daemon=True
            ),
        ]
        for reader in readers:
            reader.start()

        def send_code():
            try:
                self.process.stdin.write((prelude + "\n" + code).encode())
            except (BrokenPipeError, OSError):
                pass
            finally:
                with contextlib.suppress(BrokenPipeError, OSError):
                    self.process.stdin.close()

        writer = threading.Thread(target=send_code, daemon=True)
        writer.start()
        timed_out = False
        try:
            self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            terminate(self.process)
        finally:
            terminate(self.process)
            writer.join(timeout=5)
            for reader in readers:
                reader.join(timeout=5)
            self.last_used = time.monotonic()
        status = (
            "timeout"
            if timed_out
            else "output_limit"
            if exceeded.is_set()
            else "completed"
            if self.process.returncode == 0
            else "failed"
        )
        response = {
            "status": status,
            "exit_code": self.process.returncode,
            "elapsed_s": round(time.monotonic() - started, 2),
            "stdout": stdout_path.read_text(errors="replace")[:16000],
            "stderr": stderr_path.read_text(errors="replace")[-4000:],
            "stdout_truncated": stdout_path.stat().st_size > 16000,
            "stderr_truncated": stderr_path.stat().st_size > 4000,
            "artifacts": {
                "directory": str(run_dir),
                "stdout": str(stdout_path),
                "stderr": str(stderr_path),
            },
        }
        if status != "completed":
            response["phase"] = "execution"
            response["code_executed"] = None
            response["error"] = {
                "timeout": f"Execution exceeded timeout_s={timeout}.",
                "output_limit": f"Execution exceeded the {MAX_OUTPUT}-byte output limit.",
                "failed": f"Python exited with code {self.process.returncode}; inspect stderr.",
            }[status]
            response["next"] = (
                "Earlier actions may have completed before the error. Read the current page and inspect its receipt or saved state without navigating, clicking, submitting, or reloading. Keep the same tab and URL. Do not rerun the whole code block: a later exception does not undo an earlier write. If the outcome remains unclear, report uncertainty instead of submitting again. Python variables are fresh on each call."
            )
            if (
                status == "failed"
                and response["stderr"]
                .strip()
                .startswith("browser-harness: connecting to ")
                and "\n" not in response["stderr"].strip()
            ):
                response["error"] = (
                    f"Browser harness exited with code {self.process.returncode}; its only diagnostic concerns the driver connection. Execution remains uncertain."
                )
                response["next"] += (
                    " Use doctor to inspect browser availability; do not switch browsers or URLs to recover."
                )
            if "NameError:" in response["stderr"]:
                response["next"] += (
                    " After inspecting the page, call browser_exec with op=help, topic=overview for supported helper names; do not guess replacements."
                )
        parts = []
        if images_file.exists():
            from PIL import Image

            try:
                image_paths = json.loads(images_file.read_text())
                if not isinstance(image_paths, list):
                    raise ValueError("Expected screenshot list")
            except (OSError, ValueError):
                image_paths = []
                response.setdefault("warnings", []).append(
                    "Screenshot metadata could not be read; inspect saved artifacts."
                )
            for path in image_paths[-2:]:
                try:
                    with Image.open(path) as im:
                        im.thumbnail((1280, 1280))
                        output = io.BytesIO()
                        im.convert("RGB").save(output, format="JPEG", quality=65)
                        if len(output.getvalue()) <= 250000:
                            parts.append(
                                {
                                    "type": "image",
                                    "mimeType": "image/jpeg",
                                    "data": base64.b64encode(
                                        output.getvalue()
                                    ).decode(),
                                }
                            )
                except (OSError, ValueError, TypeError):
                    response.setdefault("warnings", []).append(
                        "A screenshot could not be encoded; inspect its saved artifact."
                    )
        return {"response": response, "parts": parts}


class Worker:
    def __init__(self, state, cdp=None, headed=False, idle=900):
        self.state, self.cdp, self.headed, self.idle = state, cdp, headed, idle
        self.sessions, self.jobs = {}, {}
        self.live_bindings = {}
        self.lock = threading.RLock()
        self.stopping = threading.Event()
        threading.Thread(target=self.reap, daemon=True).start()

    def reap(self):
        while not self.stopping.wait(10):
            with self.lock:
                names = list(self.sessions)
                self.jobs = {
                    k: v
                    for k, v in self.jobs.items()
                    if not v["done"].is_set() or time.monotonic() - v["created"] < 900
                }
            for name in names:
                self.close_session(name, idle_before=time.monotonic() - self.idle)

    def close_session(self, name, idle_before=None):
        with self.lock:
            session = self.sessions.get(name)
            if session is None:
                return True
            if idle_before is not None and session.last_used >= idle_before:
                return True
            if not session.lock.acquire(False):
                return False
        try:
            session.close()
        finally:
            with self.lock:
                del self.sessions[name]
                session.lock.release()
        return True

    def shutdown(self):
        self.stopping.set()
        with self.lock:
            for session in self.sessions.values():
                session.close()

    def call(self, data):
        op = data.get("op", "exec")
        name = data.get("session", "default")
        if not isinstance(name, str) or not NAME.fullmatch(name):
            raise ValueError("Invalid session name")
        if op == "doctor":
            try:
                chrome_binary()
                isolated_launch = "available"
            except (OSError, RuntimeError) as exc:
                isolated_launch = str(exc)
            return {
                "response": {
                    "status": "ready",
                    "harness": importlib.metadata.version("browser-harness"),
                    "mode": "attached" if self.cdp else "managed",
                    "sessions": list(self.sessions),
                    "isolated_launch": isolated_launch,
                    "authority": "Python executes as the worker OS user; this worker is not a sandbox.",
                }
            }
        if op == "discover":
            return {"response": browser_connection.discover()}
        if op == "status":
            with self.lock:
                session = self.sessions.get(name)
                return {
                    "response": {
                        "status": "connected"
                        if session and session.connected
                        else "open"
                        if session
                        else "connection_required"
                        if name in self.live_bindings
                        else "absent",
                        "session": name,
                        "mode": "live"
                        if name in self.live_bindings
                        else "attached"
                        if session and session.attached
                        else "managed",
                        "browser": self.live_bindings.get(name),
                        "last_requested_target_id": session.selected_target
                        if session
                        else None,
                        "next": "Call tabs to refresh browser observations; this status describes the session binding, not browser liveness.",
                    }
                }
        if op in ("close", "disconnect"):
            if not self.close_session(name):
                raise ValueError(
                    "Session busy; wait for its current execution before closing"
                )
            return {
                "response": {
                    "status": "disconnected" if op == "disconnect" else "closed",
                    "session": name,
                    "next": "The driver is released. Borrowed browser and tabs remain open. Connect explicitly before doing more work in your browser.",
                }
            }
        live_endpoint = None
        browser = data.get("browser", "chrome")
        if op == "connect":
            if not isinstance(browser, str):
                raise ValueError("browser must be a browser ID from discover")
            with self.lock:
                if name not in self.live_bindings and len(self.live_bindings) >= 256:
                    raise ValueError(
                        "Live session binding capacity reached; restart the runtime before creating more names"
                    )
                existing = self.sessions.get(name)
                if existing and existing.live_browser != browser:
                    raise ValueError(
                        "This session already owns a different browser. Use a new session or explicitly disconnect it first; identity was not switched."
                    )
                self.live_bindings[name] = browser
            try:
                live_endpoint = browser_connection.resolve(browser)
            except (OSError, ValueError) as exc:
                if isinstance(exc, PermissionError):
                    reason = "OS privacy settings deny access to the browser endpoint"
                    recovery = "Allow the Gents host application access in OS privacy settings, then call discover."
                elif isinstance(exc, FileNotFoundError):
                    reason = "Browser has not published a DevToolsActivePort endpoint"
                    recovery = browser_connection.SETUP
                else:
                    reason = (
                        str(exc)
                        if isinstance(exc, ValueError)
                        else f"Cannot read browser endpoint (OS error {exc.errno})"
                    )
                    recovery = "Call discover and use a valid available browser ID; repair invalid remote-debugging setup before reconnecting."
                return failure(
                    "connection_required",
                    reason,
                    "connection",
                    False,
                    recovery,
                )
        code, timeout = data.get("code"), data.get("timeout_s", 120)
        if op in ("connect", "tabs"):
            code = "import json\nprint(json.dumps(list_tabs()))"
        if op == "read":
            code = "import json\nprint(json.dumps(js(\"(() => { const html=document.documentElement.outerHTML; if(new TextEncoder().encode(html).length>1000000) throw new Error('DOM exceeds 1 MB; use exec with a scoped selector'); return {url:location.href,html}; })()\")))"
        if (
            op not in ("exec", "connect", "tabs", "read")
            or not isinstance(code, str)
            or not code.strip()
            or len(code.encode()) > 65536
        ):
            raise ValueError("exec requires 1-65536 bytes of Python code")
        if type(timeout) is not int or not 1 <= timeout <= 300:
            raise ValueError("timeout_s must be an integer between 1 and 300")
        with self.lock:
            existing = self.sessions.get(name)
            if op != "connect" and name in self.live_bindings and not existing:
                return failure(
                    "connection_required",
                    "Live browser session is disconnected or expired",
                    "connection",
                    False,
                    "Call connect explicitly using the same session and browser ID. This task was not moved to an isolated browser.",
                )
            if op == "connect" and existing and existing.live_browser != browser:
                raise ValueError(
                    "This session already owns a different browser. Use a new session or explicitly disconnect it first; identity was not switched."
                )
            if op in ("tabs", "read") and not existing:
                raise ValueError(
                    "Session absent. For the user's running browser, call discover then connect. For a public URL, call exec in this session with code=goto_url('https://...'); assert wait_for_element('body'), then call read in the SAME session. read does not navigate or accept a url argument."
                )
            target = None
            if op in ("exec", "read"):
                if data.get("target_id") is not None and not existing:
                    raise ValueError(
                        "Session absent; start it before selecting a tab from tabs"
                    )
                if existing:
                    if existing.live_browser and not existing.connected:
                        raise ValueError(
                            "Live browser connection has not completed. Call connect, approve Chrome's dialog, then inspect tabs before acting."
                        )
                    target = (
                        data["target_id"]
                        if data.get("target_id") is not None
                        else existing.selected_target
                    )
                    if (
                        data.get("target_id") is not None
                        or target is not None
                        or existing.live_browser
                    ) and (
                        not isinstance(target, str)
                        or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", target)
                    ):
                        raise ValueError(
                            "Choose target_id from connect/tabs before acting on a live browser; do not guess the user's current tab."
                        )
            if len(self.jobs) >= 64:
                completed = sorted(
                    (v["created"], k)
                    for k, v in self.jobs.items()
                    if v["done"].is_set()
                )
                if completed:
                    del self.jobs[completed[0][1]]
            if len(self.jobs) >= 64:
                raise ValueError(
                    "Job capacity reached; wait for completed jobs to expire"
                )
            if name not in self.sessions:
                if len(self.sessions) >= 8:
                    raise ValueError(
                        "Eight sessions already open; close one before starting another"
                    )
                self.sessions[name] = Session(
                    self.state,
                    name,
                    live_endpoint or self.cdp,
                    self.headed,
                    live_browser=browser if op == "connect" else None,
                )
            session = self.sessions[name]
            if not session.lock.acquire(False):
                raise ValueError(
                    "Session busy; use a distinct session name for concurrent work"
                )
            job = secrets.token_hex(16)
            selected = (
                session.root / "runs" / job / "target-selected" if target else None
            )
            if target:
                code = (
                    f"assert any(t['targetId'] == {target!r} for t in list_tabs()), 'Selected tab closed; call tabs and choose target_id explicitly'\nswitch_tab({target!r})\n_pathlib.Path({str(selected)!r}).touch()\n"
                    + code
                )
            record = {
                "done": threading.Event(),
                "created": time.monotonic(),
                "result": None,
            }
            self.jobs[job] = record

        def execute():
            try:
                record["result"] = session.execute(code, timeout, job)
                response = record["result"]["response"]
                response["session"] = name
                if response["status"] != "completed":
                    response["next_call"] = {
                        "op": "read"
                        if op == "exec" and response.get("phase") == "execution"
                        else "tabs",
                        "session": name,
                    }
                    if "Session with given id not found" in response.get("stderr", ""):
                        response["next_call"] = {"op": "tabs", "session": name}
                        response["next"] = (
                            "The selected tab session ended; the failed command was not replayed on another tab. Earlier actions may have completed. Call tabs, explicitly select the intended target_id, and inspect before retrying writes."
                        )
                if selected:
                    if selected.is_file():
                        session.selected_target = target
                    elif response.get("phase") != "startup":
                        response.update(
                            status="failed",
                            phase="target",
                            code_executed=False,
                            error="Requested tab could not be selected; no model code ran. "
                            + response.get("stderr", "")[-2000:],
                            next="Call tabs and choose a valid target_id explicitly. The previous pin is unchanged.",
                            next_call={"op": "tabs", "session": name},
                        )
                if op == "read":
                    response["code_executed"] = False
                    if "DOM exceeds 1 MB" in response.get("stderr", ""):
                        response["phase"] = "read"
                        response["error"] = (
                            "Top-level DOM exceeds the 1 MB capture limit"
                        )
                        response["next"] = (
                            "The DOM is too large for read; max_chars and selector apply after capture. Use exec to inspect a small section. No model-written code ran."
                        )
                        response["next_call"] = {
                            "op": "exec",
                            "session": name,
                            "code": 'print(js("document.body?.innerText.slice(0, 6000)"))',
                        }
                if op == "read" and response["status"] == "completed":
                    response["page"] = json.loads(
                        Path(response["artifacts"]["stdout"]).read_text()
                    )
                    response.pop("stdout", None)
                    response.pop("stdout_truncated", None)
                if op in ("connect", "tabs"):
                    if response["status"] == "completed":
                        tabs = json.loads(
                            Path(response["artifacts"]["stdout"]).read_text()
                        )
                        if not isinstance(tabs, list):
                            raise ValueError("Invalid browser tab list")
                        session.connected = True
                        response.pop("stdout", None)
                        response.pop("stdout_truncated", None)
                        response.update(
                            status="connected" if op == "connect" else "completed",
                            tabs=tabs[:200],
                            tabs_total=len(tabs),
                            tabs_truncated=len(tabs) > 200,
                            browser=session.live_browser,
                            session=name,
                            next="Use target_id from these tabs with exec to read or act on the requested page. Do not infer active tab or signed-in profile from tab order. Reuse this session; disconnect releases the driver without closing Chrome.",
                        )
                    else:
                        response.update(
                            phase="connection",
                            code_executed=False,
                            error=response.get("stderr", "")[-2000:]
                            or response.get("error", "Browser connection failed"),
                            next_call={"op": "disconnect", "session": name},
                            next="No model code was submitted. Approve Chrome's connection dialog if present. If denied, disconnected or timed out, disconnect this session, then connect again only when the user is ready. Never fall back to a signed-out browser for this task.",
                        )
            except Exception as exc:
                record["result"] = failure(
                    "failed",
                    exc,
                    "execution",
                    None,
                    "Execution outcome is uncertain. Inspect browser state before retrying any write.",
                )
            finally:
                session.lock.release()
                record["done"].set()

        try:
            threading.Thread(target=execute, daemon=True).start()
        except RuntimeError:
            with self.lock:
                del self.jobs[job]
                session.lock.release()
            raise
        return {"status": "running", "job": job}


def handler(worker, token):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def respond(self, status, value):
            body = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
            if len(body) > 1024 * 1024:
                response = value.get("response", {})
                value = failure(
                    "output_too_large",
                    "Encoded browser response exceeds the 1 MiB transport limit",
                    "output",
                    response.get("code_executed"),
                    "Use exec in the same session to print a small DOM section with js(\"document.querySelector('main').innerText.slice(0, 6000)\"). Inspect state before repeating any write.",
                    artifacts=response.get("artifacts", {}),
                )
                value["response"]["session"] = response.get("session")
                value["response"]["next_call"] = {
                    "op": "exec",
                    "session": response.get("session") or "default",
                    "code": 'print(js("document.body?.innerText.slice(0, 6000)"))',
                }
                body = json.dumps(
                    value, ensure_ascii=False, separators=(",", ":")
                ).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                self.wfile.write(body)

        def authorized(self):
            if self.headers.get("Origin") or not hmac.compare_digest(
                self.headers.get("Authorization", ""), "Bearer " + token
            ):
                self.respond(
                    403,
                    failure(
                        "error",
                        "Worker authorization required",
                        "authorization",
                        None,
                        "Rerun setup and use its returned connection file. If polling an earlier job, its outcome is unknown; inspect state before repeating a write.",
                    ),
                )
                return False
            return True

        def do_GET(self):
            if not self.authorized():
                return
            if self.path == "/health":
                self.respond(200, worker.call({"op": "doctor"}))
            elif self.path.startswith("/jobs/"):
                with worker.lock:
                    record = worker.jobs.get(self.path[6:])
                if not record:
                    self.respond(
                        404,
                        failure(
                            "error",
                            "Unknown or expired job",
                            "polling",
                            None,
                            "The execution outcome is unknown. Inspect the page before repeating a write.",
                        ),
                    )
                elif record["done"].wait(15):
                    self.respond(200, record["result"])
                else:
                    self.respond(202, {"status": "running", "job": self.path[6:]})
            else:
                self.respond(404, {"error": "Unknown route"})

        def do_POST(self):
            if not self.authorized():
                return
            if self.path == "/stop":
                self.respond(200, {"response": {"status": "stopping"}})
                threading.Thread(target=self.server.shutdown, daemon=True).start()
                return
            if self.path != "/call":
                self.respond(404, {"error": "Unknown route"})
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 100000:
                    raise ValueError("Request must be at most 100000 bytes")
                self.connection.settimeout(5)
                data = json.loads(self.rfile.read(length))
                if not isinstance(data, dict):
                    raise ValueError("Expected object")
                result = worker.call(data)
                self.respond(202 if result.get("status") == "running" else 200, result)
            except (ValueError, RuntimeError, OSError) as exc:
                self.respond(
                    400,
                    failure(
                        "error",
                        exc,
                        "admission",
                        False,
                        f"{exc} This request was not executed.",
                    ),
                )

    return Handler


def request(connection, route, body=None):
    req = Request(
        connection["endpoint"] + route,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": "Bearer " + connection["token"],
            "Content-Type": "application/json",
        },
    )
    with urlopen(req, timeout=20) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command", choices=["setup", "serve", "managed", "doctor", "stop"]
    )
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument(
        "--port",
        type=int,
        default=18765,
        help="Loopback port; must match the installed plugin network grant",
    )
    parser.add_argument(
        "--cdp",
        help="Explicit existing CDP endpoint; omitted starts dedicated Chromium",
    )
    parser.add_argument("--headed", action="store_true")
    parser.add_argument(
        "--allow-host-execution",
        action="store_true",
        help="Acknowledge Python runs with this OS user's authority",
    )
    parser.add_argument(
        "--gents-home",
        type=Path,
        help="Also allow this connection file for model tool calls in this Gents home",
    )
    parser.add_argument(
        "--gents", default="gents", help="Gents executable used with --gents-home"
    )
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("--port must be 1..65535")
    origin = f"http://127.0.0.1:{args.port}"
    if args.command == "managed" and not args.cdp:
        args.cdp = os.environ.get("GENTS_BROWSER_CDP")
    state = args.state.expanduser().resolve()
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(state, 0o700)
    connection_path = state / "connection.json"

    def setup_result(reused):
        result = {
            "status": "ready",
            "connection": str(connection_path),
            "reused": reused,
        }
        if args.gents_home:
            binding = subprocess.run(
                [
                    args.gents,
                    "plugin",
                    "dirs",
                    "add",
                    str(connection_path),
                    "--home",
                    str(args.gents_home.expanduser().resolve()),
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=30,
            )
            if binding.returncode:
                raise RuntimeError(
                    f"Gents connection permission setup failed: {binding.stderr[-2000:]}"
                )
            result["gents_binding"] = "read"
        else:
            result["gents_binding"] = "required_for_model_calls"
            result["next_command"] = [
                args.gents,
                "plugin",
                "dirs",
                "add",
                str(connection_path),
                "--home",
                "<gents-home>",
            ]
        print(json.dumps(result))

    if args.command in ("doctor", "stop"):
        print(
            json.dumps(
                request(
                    json.loads(connection_path.read_text()),
                    "/health" if args.command == "doctor" else "/stop",
                    None if args.command == "doctor" else {},
                )
            )
        )
        return
    if args.command == "setup":
        if not args.allow_host_execution:
            parser.error(
                "setup requires --allow-host-execution: the worker executes model-written Python as your OS user, like Hermes browser_exec"
            )
        with open(state / "setup.lock", "a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if connection_path.exists():
                connection = json.loads(connection_path.read_text())
                try:
                    request(connection, "/health")
                except OSError:
                    pass
                else:
                    setup_result(True)
                    return
            importlib.metadata.version("browser-harness")
            connection = {"endpoint": origin, "token": secrets.token_hex(32)}
            write_json(connection_path, connection)
            write_json(
                state / "settings.json", {"cdp": args.cdp, "headed": args.headed}
            )
            with open(state / "worker.log", "ab", buffering=0) as log:
                proc = subprocess.Popen(
                    [
                        sys.executable,
                        str(Path(__file__).resolve()),
                        "serve",
                        "--port",
                        str(args.port),
                        "--state",
                        str(state),
                    ],
                    stdin=subprocess.DEVNULL,
                    stdout=log,
                    stderr=log,
                    start_new_session=not os.environ.get("GENTS_MANAGED_SERVICE"),
                )
            until = time.monotonic() + 10
            while time.monotonic() < until and proc.poll() is None:
                try:
                    request(connection, "/health")
                except OSError:
                    time.sleep(0.1)
                else:
                    write_json(state / "worker.json", {"pid": proc.pid})
                    setup_result(False)
                    return
            terminate(proc)
            raise RuntimeError(
                f"Worker failed to start. Inspect {state / 'worker.log'}; port {args.port} may already be in use."
            )
    if args.command == "managed":
        if not args.allow_host_execution:
            parser.error("managed requires --allow-host-execution")
        os.environ["GENTS_MANAGED_SERVICE"] = "1"
        importlib.metadata.version("browser-harness")
        write_json(
            connection_path, {"endpoint": origin, "token": secrets.token_hex(32)}
        )
        write_json(state / "settings.json", {"cdp": args.cdp, "headed": args.headed})
    with open(state / "worker.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        connection = json.loads(connection_path.read_text())
        settings = json.loads((state / "settings.json").read_text())
        worker = Worker(state, **settings)
        server = ThreadingHTTPServer(
            ("127.0.0.1", args.port), handler(worker, connection["token"])
        )

        def stop(*_):
            threading.Thread(target=server.shutdown, daemon=True).start()

        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        if args.command == "managed":
            write_json(
                state / "worker.json", {"pid": os.getpid(), "pgid": os.getpgrp()}
            )
            print(json.dumps(connection), flush=True)
        try:
            server.serve_forever()
        finally:
            server.server_close()
            worker.shutdown()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps({"status": "error", "error": str(error)}), file=sys.stderr)
        sys.exit(1)

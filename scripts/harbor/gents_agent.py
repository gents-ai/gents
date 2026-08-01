"""Harbor custom agent that runs the native Gents runtime inside a task container.

Use this agent by import path from the repository root::

    harbor run ... --agent scripts.harbor.gents_agent:GentsAgent

The adapter deliberately installs Gents *inside* the task environment. Native
filesystem and shell tools therefore operate on the same ``/app`` tree that the
Harbor verifier inspects.
"""

from __future__ import annotations

import json
import os
import re
import shlex
import tempfile
from pathlib import Path
from typing import Any

import certifi

from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext


class GentsAgent(BaseAgent):
    """Run one Harbor instruction through a durable Gents request."""

    SUPPORTS_ATIF = True
    _REMOTE_BINARY = "/usr/local/bin/gents"
    _REMOTE_REAL_BINARY = "/usr/local/libexec/gents-harbor"
    _REMOTE_RUNNER = "/usr/local/bin/run-gents-harbor"
    _REMOTE_CA_BUNDLE = "/tmp/gents-harbor-ca-bundle.pem"
    _REMOTE_GLIBC_BUNDLE = "/tmp/gents-harbor-glibc.tar.gz"
    _REMOTE_GLIBC_DIR = "/usr/local/lib/gents-harbor-glibc"
    _RUNNER_SOURCE = Path(__file__).with_name("run_gents.sh")

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        docker_platform = self._env("GENTS_DOCKER_PLATFORM")
        if docker_platform:
            # Compose honors this for prebuilt images. Harbor 0.20.0 does not
            # honor it for buildx, whose resolver runs after agent creation.
            os.environ["DOCKER_DEFAULT_PLATFORM"] = docker_platform
            from harbor.environments.docker import docker as harbor_docker
            from harbor.environments.docker import utils as harbor_docker_utils

            async def configured_docker_platform() -> str:
                return docker_platform

            harbor_docker.default_docker_platform = configured_docker_platform
            harbor_docker_utils.default_docker_platform = configured_docker_platform

    @staticmethod
    def name() -> str:
        return "gents"

    def version(self) -> str | None:
        return self._env("GENTS_VERSION") or "source"

    def _env(self, name: str, default: str | None = None) -> str | None:
        value = self.extra_env.get(name)
        if value is None:
            value = os.environ.get(name)
        if value is None:
            return default
        value = value.strip()
        return value or default

    @staticmethod
    def _require_success(command: str, result: Any) -> None:
        if result.return_code == 0:
            return
        stdout = (result.stdout or "")[-4_000:]
        stderr = (result.stderr or "")[-4_000:]
        raise RuntimeError(
            f"Gents Harbor command failed with exit {result.return_code}: {command}\n"
            f"stdout:\n{stdout}\nstderr:\n{stderr}"
        )

    async def _install_ca_bundle(self, environment: BaseEnvironment) -> None:
        """Provide TLS roots without invoking a package manager in every task."""
        ca_bundle = Path(certifi.where())
        if not ca_bundle.is_file():
            raise FileNotFoundError(f"Harbor CA bundle is missing: {ca_bundle}")
        await environment.upload_file(ca_bundle, self._REMOTE_CA_BUNDLE)
        command = f"test -s {shlex.quote(self._REMOTE_CA_BUNDLE)}"
        result = await environment.exec(command=command, user="root")
        self._require_success("install Harbor CA bundle", result)

    async def _install_uploaded_binary(
        self, environment: BaseEnvironment, binary_path: Path
    ) -> None:
        if not binary_path.is_file():
            raise ValueError(f"GENTS_BINARY_PATH is not a file: {binary_path}")
        upload_path = "/tmp/gents-harbor-upload"
        await environment.upload_file(binary_path, upload_path)
        loader_check = await environment.exec(
            command=(
                "test -x /lib64/ld-linux-x86-64.so.2 || "
                "test -x /lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
            )
        )
        if loader_check.return_code == 0:
            command = (
                f"install -m 0755 {shlex.quote(upload_path)} "
                f"{shlex.quote(self._REMOTE_BINARY)}"
            )
            result = await environment.exec(command=command, user="root")
            self._require_success(command, result)
            return

        bundle_path = self._env("GENTS_GLIBC_BUNDLE_PATH")
        if not bundle_path:
            raise RuntimeError(
                "The task image has no glibc loader. Set GENTS_GLIBC_BUNDLE_PATH "
                "to the Bullseye x86_64 compatibility bundle."
            )
        local_bundle = Path(bundle_path)
        if not local_bundle.is_file():
            raise ValueError(
                f"GENTS_GLIBC_BUNDLE_PATH is not a file: {local_bundle}"
            )
        await environment.upload_file(local_bundle, self._REMOTE_GLIBC_BUNDLE)
        command = f"""
set -eu
install -d -m 0755 {shlex.quote(self._REMOTE_GLIBC_DIR)} /usr/local/libexec
tar -xzf {shlex.quote(self._REMOTE_GLIBC_BUNDLE)} -C {shlex.quote(self._REMOTE_GLIBC_DIR)}
install -m 0755 {shlex.quote(upload_path)} {shlex.quote(self._REMOTE_REAL_BINARY)}
printf '%s\\n' '#!/bin/sh' 'exec {self._REMOTE_GLIBC_DIR}/ld-linux-x86-64.so.2 --library-path {self._REMOTE_GLIBC_DIR} {self._REMOTE_REAL_BINARY} "$@"' > {shlex.quote(self._REMOTE_BINARY)}
chmod 0755 {shlex.quote(self._REMOTE_BINARY)}
""".strip()
        result = await environment.exec(command=command, user="root")
        self._require_success("install Gents with glibc compatibility bundle", result)

    async def _install_release_binary(
        self, environment: BaseEnvironment, release_url: str
    ) -> None:
        quoted_url = shlex.quote(release_url)
        command = f"""
set -eu
for command_name in curl tar; do
  command -v "$command_name" >/dev/null 2>&1 || {{
    echo "release install requires $command_name" >&2
    exit 1
  }}
done
install_dir=$(mktemp -d /tmp/gents-harbor-release.XXXXXX)
trap 'rm -rf "$install_dir"' EXIT
curl -fsSL {quoted_url} -o "$install_dir/gents.tar.gz"
tar -xzf "$install_dir/gents.tar.gz" -C "$install_dir"
binary=$(find "$install_dir" -type f -name gents -perm -u+x -print -quit)
test -n "$binary"
install -m 0755 "$binary" {shlex.quote(self._REMOTE_BINARY)}
""".strip()
        result = await environment.exec(
            command=command,
            user="root",
            timeout_sec=300,
        )
        self._require_success("install Gents release", result)

    async def setup(self, environment: BaseEnvironment) -> None:
        await self._install_ca_bundle(environment)

        binary_path = self._env("GENTS_BINARY_PATH")
        release_url = self._env("GENTS_RELEASE_URL")
        if binary_path:
            await self._install_uploaded_binary(environment, Path(binary_path))
        elif release_url:
            await self._install_release_binary(environment, release_url)
        else:
            raise ValueError(
                "Set GENTS_BINARY_PATH to a host Linux gents binary or "
                "GENTS_RELEASE_URL to a gents Linux release tarball"
            )

        if not self._RUNNER_SOURCE.is_file():
            raise FileNotFoundError(f"Harbor runner is missing: {self._RUNNER_SOURCE}")
        runner_upload = "/tmp/run-gents-harbor-upload"
        await environment.upload_file(self._RUNNER_SOURCE, runner_upload)
        install_runner = (
            f"install -m 0755 {shlex.quote(runner_upload)} "
            f"{shlex.quote(self._REMOTE_RUNNER)}"
        )
        result = await environment.exec(command=install_runner, user="root")
        self._require_success(install_runner, result)

        version_result = await environment.exec(command=f"{self._REMOTE_BINARY} version")
        self._require_success("gents version", version_result)
        detected_version = (version_result.stdout or "").strip()
        if detected_version:
            self.logger.debug("Installed %s", detected_version)

        help_result = await environment.exec(
            command=f"{self._REMOTE_BINARY} trace project --help"
        )
        self._require_success("gents trace project --help", help_result)
        help_text = f"{help_result.stdout or ''}\n{help_result.stderr or ''}"
        if "atif" not in help_text or "native-json" not in help_text:
            raise RuntimeError(
                "The installed Gents binary does not include Harbor ATIF support; "
                "build this branch or use a release containing PR #988"
            )

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        if not self.model_name:
            raise ValueError("Harbor --model is required for the Gents agent")

        model_name = self._env("GENTS_MODEL", self.model_name) or self.model_name
        inference_url = self._env("GENTS_INFERENCE_URL") or self._env(
            "OPENAI_BASE_URL"
        )
        if not inference_url:
            raise ValueError(
                "Set GENTS_INFERENCE_URL or OPENAI_BASE_URL to the OpenAI-compatible "
                "inference endpoint, including /v1"
            )

        session_slug = re.sub(r"[^A-Za-z0-9_.-]+", "-", self.session_id or "trial")
        instruction_path = f"/tmp/gents-harbor-{session_slug}.instruction.md"
        with tempfile.TemporaryDirectory(prefix="gents-harbor-instruction-") as temp_dir:
            local_instruction = Path(temp_dir) / "instruction.md"
            local_instruction.write_text(instruction)
            await environment.upload_file(local_instruction, instruction_path)

        chmod_result = await environment.exec(
            command=f"chmod 0644 {shlex.quote(instruction_path)}", user="root"
        )
        self._require_success("prepare Gents instruction", chmod_result)

        request_timeout = int(self._env("GENTS_REQUEST_TIMEOUT_SECS", "1800") or 1800)
        run_env = {
            "GENTS_BINARY": self._REMOTE_BINARY,
            "GENTS_HOME": f"/tmp/gents-harbor-{session_slug}",
            "GENTS_INSTRUCTION_FILE": instruction_path,
            "GENTS_INFERENCE_URL": inference_url.rstrip("/"),
            "GENTS_MODEL": model_name,
            "GENTS_TEMPERATURE": self._env("GENTS_TEMPERATURE", "1.0") or "1.0",
            "GENTS_TOP_P": self._env("GENTS_TOP_P", "1.0") or "1.0",
            "GENTS_TOP_K": self._env("GENTS_TOP_K", "") or "",
            "GENTS_MAX_TOKENS": self._env("GENTS_MAX_TOKENS", "32768") or "32768",
            "GENTS_MAX_TURNS": self._env("GENTS_MAX_TURNS", "250") or "250",
            "GENTS_RETRY_MAX_TRANSPORT": self._env(
                "GENTS_RETRY_MAX_TRANSPORT", "3"
            )
            or "3",
            "GENTS_REQUEST_TIMEOUT_SECS": str(request_timeout),
            "GENTS_COMMAND_TIMEOUT_SECS": self._env(
                "GENTS_COMMAND_TIMEOUT_SECS", "900"
            )
            or "900",
            "GENTS_SERVER_STARTUP_TIMEOUT_SECS": self._env(
                "GENTS_SERVER_STARTUP_TIMEOUT_SECS", "120"
            )
            or "120",
            "GENTS_TOOL_ROOT": self._env("GENTS_TOOL_ROOT", "/app") or "/app",
            "GENTS_API_KEY": self._env("GENTS_API_KEY", "no-key") or "no-key",
            "SSL_CERT_FILE": self._REMOTE_CA_BUNDLE,
        }
        result = await environment.exec(
            command=self._REMOTE_RUNNER,
            cwd=run_env["GENTS_TOOL_ROOT"],
            env=run_env,
            timeout_sec=request_timeout + 180,
        )
        self._require_success(self._REMOTE_RUNNER, result)

        context.metadata = {
            **(context.metadata or {}),
            "gents": {
                "model": model_name,
                "inference_url": inference_url,
                "temperature": float(run_env["GENTS_TEMPERATURE"]),
                "top_p": float(run_env["GENTS_TOP_P"]),
                "max_turns": int(run_env["GENTS_MAX_TURNS"]),
                "retry_max_transport": int(run_env["GENTS_RETRY_MAX_TRANSPORT"]),
            },
        }

    def populate_context_post_run(self, context: AgentContext) -> None:
        trajectory_path = self.logs_dir / "trajectory.json"
        if not trajectory_path.is_file():
            self.logger.warning("Gents did not emit %s", trajectory_path)
            return
        try:
            trajectory = json.loads(trajectory_path.read_text())
        except (OSError, json.JSONDecodeError):
            self.logger.exception("Failed to read Gents ATIF trajectory")
            return

        request_path = self.logs_dir / "request.json"
        request: dict[str, Any] = {}
        if request_path.is_file():
            try:
                parsed = json.loads(request_path.read_text())
                if isinstance(parsed, dict):
                    request = parsed
            except (OSError, json.JSONDecodeError):
                self.logger.debug("Failed to read Gents request metadata", exc_info=True)

        context.metadata = {
            **(context.metadata or {}),
            "gents": {
                **((context.metadata or {}).get("gents") or {}),
                "request_id": request.get("request_id"),
                "session_id": trajectory.get("session_id"),
                "trajectory_id": trajectory.get("trajectory_id"),
                "total_steps": (trajectory.get("final_metrics") or {}).get(
                    "total_steps"
                ),
            },
        }

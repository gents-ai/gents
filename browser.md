# Browser use

## Native managed worker

With a Gents build supporting pack services, install this pack with
`--grant-authority` and call `browser_exec` without `connection`. Gents launches
`worker/browser_worker.py managed` through its existing managed-process owner,
waits for readiness, and supplies the connection internally. `op=help` remains
available without starting the service. The worker and its browser/driver children
stay in Gents's process group; they are terminated when the owning executor drops.
The service lasts for that runtime executor, while named browser sessions retain
their existing idle cleanup. Calls do not automatically replay or restart a dead
service. Artifacts remain under `<gents-home>/plugin-services/run-*/worker/`.

This mode still requires `uv`, Python 3.13 (uv may install it), and Chrome/Chromium.
It bundles source and a dependency lock, not those runtime binaries. Port 18765
must be free; multiple independent runtimes need separate network environments
until the transport supports configurable endpoints. The explicit external-worker
setup below remains available when `connection` is supplied.

Ask the model to connect to your running browser. `op=discover` checks local
browser setup without making a debugging connection. `op=connect` with its
browser ID opens the connection and returns existing tabs. In Chrome 144+,
enable `chrome://inspect/#remote-debugging` once, then approve Chrome's Allow
dialog. The first `exec` or `read` names the requested `target_id`; subsequent
calls reuse that task session. `disconnect` leaves Chrome and all user tabs open.
This does not copy profiles, read cookie stores, or infer signed-in profile names.

For custom installations, set `GENTS_BROWSER_ROOTS` to a JSON object mapping
browser IDs to user-data directories before starting Gents. It replaces default
discovery. For operator-configured CDP attachment, `GENTS_BROWSER_CDP` or external
setup `--cdp` remains available; this mode starts on a separate task tab.
Never point unattended evals at personal browsers. The live connection test uses
an isolated, separately owned Chrome with a fixture login and fixed discovery roots.

Test native ownership with `worker/tests/live_model.py --managed` and the other
live-model arguments below. This skips external setup and exercises runtime startup.


Hermes-style browser execution for Gents: one `gents/browser_exec` plugin runs
Python with Browser Use's browser-harness helpers against a persistent Chromium
session. Gents keeps inference, conversations, and task completion. No additional
model API key is required. This is a capability pack, not a separate browser agent.

## Model-led installation

Prerequisites: a current Gents runtime with plugin HTTP rounds and directory
bindings, `uv`, and macOS or Linux. Managed mode needs Chrome/Chromium. Existing
local or remote CDP endpoints can be attached explicitly. The Python worker runs
as its OS user with that user's filesystem and network access, like Hermes's
terminal-capable browser mode. Use an isolated OS environment when that authority
is inappropriate. WASM limits bound the adapter, not the external Python worker.

From this pack's directory, choose a private, persistent state directory outside
the checkout. Models can execute this setup after the operator has authorized
host code execution. It installs the locked dependencies, starts the worker,
and returns JSON containing `status` and a `connection` file path, never its token:

```sh
uv run --locked --project worker python worker/browser_worker.py setup \
  --state ~/.local/share/gents-browser --allow-host-execution --gents-home ~/.gents
uv run --locked --project worker python worker/browser_worker.py doctor \
  --state ~/.local/share/gents-browser
gents pack build .
gents pack install . --grant-authority --inference-slot web_researcher=PROFILE_ID
```

Registry users install `gents/web --grant-authority` and run setup from
the downloaded pack assets. Setup is idempotent: a healthy worker is reused.
It does not silently change an existing worker's browser mode. To change modes,
stop the worker before rerunning setup. Setup's `--headed` launches visible
managed browsers; `--cdp http://127.0.0.1:9222` or `--cdp wss://...` borrows an
explicit browser endpoint instead. It never enables debugging on a personal
browser or copies cookies automatically. Cloud provisioning is external: supply
the provider's CDP endpoint. The pack does not provision or bill cloud browsers.

`--gents-home` allows only the connection file through Gents's plugin folder
controls, so unattended model calls do not wait for an interactive approval.
Use `--gents /path/to/current/gents` to select the runtime binary. Without a home,
setup returns `gents_binding: required_for_model_calls` and the exact next command.
`doctor` checks worker availability, not the Gents file binding. Native help works
offline: call `browser_exec` with `op: "help"`, optional `topic`, and no connection.

Add `{"plugin":"gents/browser_exec"}` to the desired Tools document's
`integrations.plugins` list. The existing Gents session selects the model.
Use the setup-returned connection file as `connection`; Gents admits that file
through the plugin's read-only directory binding. An operator may allow its
private directory through the existing folder controls. Do not paste the file's
contents into prompts. The worker binds only `127.0.0.1:18765`; the plugin's
network grant names exactly that endpoint. One worker can occupy that port per
network namespace. Separate mutually untrusted operators using separate OS
environments, not session names.

Example tool arguments:

```json
{
  "connection": "/absolute/private/state/connection.json",
  "session": "invoice-review-42",
  "code": "goto_url('https://example.com')\nprint(page_info())\nprint(js('document.title'))\ncapture_screenshot()",
  "timeout_s": 120
}
```

## Execution and results

The WASM adapter submits one job and polls its handle through host HTTP rounds.
It does not resubmit execution after a transport failure. The worker serializes
each named session, allows eight sessions, caps execution at 300 seconds, limits
stdout/stderr to 2 MiB each, and returns compact text plus up to two bounded image
parts. Full output is saved under `sessions/<name>/runs/<job>/`. Artifacts stay
local; remote consumers need their existing file transfer mechanism. Tool images
are only visible on Gents providers supporting image tool results; other providers
receive the runtime's image note. Use DOM inspection when vision is unavailable.

Errors return `status`, `error`, `phase`, `code_executed`, and `next` directly to
the model. `code_executed: false` means this call's code never ran; `null` means
the outcome is unknown and writes must not be blindly retried. Authorization and
expired-job responses retain their recovery details through the WASM adapter.
Execution stderr preserves its final 4,000 characters so the exception remains
visible after long logs; artifact paths point to the full bounded output.

Browser state and workspace files survive calls; Python variables do not. Managed
browser profiles survive close/reopen, including logins. Closing or idle cleanup
is not a profile wipe: the default idle limit is 15 minutes.
Python's working directory is the session workspace; `agent_helpers.py` there is
imported on subsequent calls. Save extraction batches there and aggregate in code.
Managed Chrome starts inside the asynchronous job, with a separate 60-second
startup deadline and a CDP readiness probe. Slow startup does not block worker
health checks or other sessions. Startup failures explicitly report that model
code never ran and identify the Chrome log. Model code is never auto-replayed.
An attached browser gets a dedicated task tab; the worker never kills it or the
borrowed browser. Attached tabs may remain after cleanup and can be closed
explicitly by the task. If an attached driver restarts, it acquires a new task tab
before running model code, avoiding the user's active tab. A timeout can leave completed external actions behind;
inspect state before retrying. Gents cancellation stops the plugin polling; the
worker's independent execution deadline still bounds a submitted job. Immediate
propagation of cancellation to the worker is not yet wired.

Stop with `uv run --locked --project worker python worker/browser_worker.py stop
--state ~/.local/share/gents-browser` (one line). SIGTERM also closes its managed
browsers and driver processes. Keep state for reusable profiles, or delete the private state
directory after stopping if profiles/artifacts should be removed. Abrupt SIGKILL
or a machine crash may leave browser processes requiring operator cleanup.

## Validation and provenance

```sh
cargo test --workspace --manifest-path plugins/web_runtime/Cargo.toml
uv run --locked --project worker python -m unittest discover -s worker/tests
uv run --locked --project worker python worker/tests/live_browser.py
uv run --locked --project worker python worker/tests/live_regressions.py
python3 worker/tests/live_fallback.py --gents /path/to/current/gents
uv run --locked --project worker python worker/tests/live_gents.py --gents /path/to/current/gents
gents pack check .
gents pack test .
```

The live tests use a temporary managed browser and a local fixture site, never
personal accounts. It covers form interaction, persistent state, screenshot
content, timeout recovery, and non-destructive CDP attachment. The Gents integration
test covers setup reuse and the installed WASM/host HTTP path. It needs port 18765
free. Lifecycle and authorization tests use test doubles.
The pack suite also verifies install/remove and plugin input validation.

### Live model integration

This creates a fresh Gents home, installs the pack, starts a runtime with only
the browser task capability, and sends real model requests. It checks native help,
three-page invoice filtering, a follow-up turn with separate tabs and a delayed
dependent form, CSV upload, and recovery after an intentional timeout and Python
exception. The exception task verifies that the model receives and interprets
the error's phase, uncertain execution state, and recovery guidance. Form and
upload outcomes are checked against server-side state, including exactly-once
submission. It does not rely solely on the model claiming success.

```sh
uv run --locked --project worker python worker/tests/live_model.py --managed \
  --gents /path/to/current/gents --endpoint http://workstation-1:8000/v1
```

The first advertised model is used unless `--model` is given. Port 18765 must be
free; the fixture and Gents server use temporary ports. `--output <fresh-dir>`
selects the evidence directory; otherwise a new directory is created in `/tmp`.
Prompts, responses, tool traces, fixture state, and `report.json` stay there.
The test stops its server, worker, and browsers when finished. It requires real
inference and is separate from the offline CI suite. It does not validate visual
reasoning on providers that cannot receive tool-result images.

Add `--research` to run public-web benchmark research and a follow-up source
audit instead of the fixture tasks. It starts from official project pages,
requires saved source links and short quotes observed in browser output, and
retains traces for factual review. This checks execution and evidence collection;
it does not treat the model's synthesis as independently verified truth.

The running-browser flow was validated on 2026-10-16 with fresh installed packs
and real `GLM-5.3-Flash-NVFP4` inference on workstation-1 and workstation-2.
Both verified authenticated reading, live editing, preserved tab IDs/URLs and
browser survival after disconnect, plus native process-group cleanup. An
operator-approved connection to an ordinary running Chrome also listed its live
tabs successfully. These are functional integration tests, not a reliability
benchmark. The automated fixtures never use personal browser accounts.

Browser Use browser-harness is pinned to 0.1.13 with a dependency lock; upgrades
must rerun the live suite. This integration follows
[Hermes browser execution](https://github.com/NousResearch/hermes-agent/blob/main/tools/browser_use_cli.py)
and reuses [Browser Use's MIT-licensed harness](https://github.com/browser-use/browser-harness).
Chrome's running-browser connection uses its documented
[approval flow](https://developer.chrome.com/blog/chrome-devtools-mcp-debug-your-browser-session).

### Native Gents eval definitions

The pack ships its own [native eval suite](evals/README.md) under `evals/`, with
six acceptance cases, a browser-only subject agent, and independent fixture-state
oracles. Both nested packs can be checked with `gents pack check`. Native trial
provisioning and publishing oracle observations still need a runner bridge; these
native definitions have not yet been executed. The live model runner above remains
the working end-to-end integration test.

## Running-browser integration test

```sh
uv run --locked --project packs/gents/web/worker python \
  packs/gents/web/worker/tests/live_model.py --managed --connected \
  --gents /path/to/gents --endpoint http://workstation-1:8000/v1
```

This creates a fresh Gents home and a separately owned Chromium profile, logs into
a fixture with an HttpOnly cookie, and asks real inference to discover/connect,
read the existing authenticated tab, edit another existing tab and disconnect.
An independent owner connection verifies the exact edit, unchanged tab IDs/URLs
and surviving browser. The test does not automate Chrome's approval dialog.

Use `--managed --web-research` to exercise the unified search, HTTP fetch and
rendered-page reader against live official documentation. It checks successful
tool results and citations; source interpretation still needs review.

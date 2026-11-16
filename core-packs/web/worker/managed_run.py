import os
import runpy
import subprocess
import sys


def pin_requests(send):
    def pinned(req, **kwargs):
        method = req.get("method")
        if method and not method.startswith("Target.") and not req.get("session_id"):
            session = send({"meta": "session"}, **kwargs).get("session_id")
            if not session:
                raise RuntimeError(
                    "No selected tab session. List tabs and select a target_id."
                )
            # Explicit sessions disable the pinned harness's cross-tab recovery/replay.
            req = {**req, "session_id": session}
        return send(req, **kwargs)

    return pinned


if __name__ == "__main__":
    if sys.argv[1:] == ["--daemon"]:
        import asyncio

        from browser_harness import daemon

        async def await_selection(self, **kwargs):
            return None

        daemon.Daemon.attach_first_page = await_selection
        if daemon.already_running():
            sys.exit(0)
        open(daemon.LOG, "w").close()
        daemon._publish_own_pid()
        try:
            asyncio.run(daemon.main())
        except KeyboardInterrupt:
            pass
        finally:
            try:
                os.unlink(daemon.PID)
            except FileNotFoundError:
                pass
        sys.exit(0)

    from browser_harness import _ipc, helpers

    helpers._send = pin_requests(helpers._send)
    if os.environ.get("GENTS_LIVE_BROWSER"):
        popen = subprocess.Popen

        def spawn(argv, **kwargs):
            if argv == [sys.executable, "-m", "browser_harness.daemon"]:
                argv = [sys.executable, os.path.abspath(__file__), "--daemon"]
            return popen(argv, **kwargs)

        subprocess.Popen = spawn
    if os.environ.get("GENTS_MANAGED_SERVICE"):
        _ipc.spawn_kwargs = lambda: {}
    runpy.run_module("browser_harness.run", run_name="__main__")

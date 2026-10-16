"""Exercise live-tab failure boundaries using a disposable Chrome, without inference."""

import json
import sys
import tempfile
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from browser_worker import Session, Worker


def run(session, code, success=True):
    response = session.execute(code, 30, uuid.uuid4().hex)["response"]
    assert (response["status"] == "completed") == success, response
    return response


def main():
    with tempfile.TemporaryDirectory(prefix="gents-tab-regressions-") as root:
        owner = Session(Path(root), "owner")
        owner.env["BH_TAB_MARKER"] = "0"
        live = None
        worker = Worker(Path(root))
        try:
            owner.start()
            result = run(
                owner,
                """import json
target = current_tab()['target_id']
for tab in list_tabs():
    if tab['target_id'] != target:
        cdp('Target.closeTarget', targetId=tab['target_id'])
goto_url('chrome://version')
print(json.dumps(list_tabs()))
""",
            )
            before = json.loads(result["stdout"])
            live = Session(Path(root), "live", cdp=owner.cdp, live_browser="fixture")
            tabs = json.loads(
                run(live, "import json; print(json.dumps(list_tabs()))")["stdout"]
            )
            assert {t["target_id"] for t in tabs} == {t["target_id"] for t in before}
            target = tabs[0]["target_id"]
            run(
                owner,
                "new_tab('data:text/html,<title>Untouched</title><p>Other tab</p>')",
            )
            result = run(
                live,
                f"""switch_tab({target!r})
cdp('Target.closeTarget', targetId={target!r})
js("document.title='WRONG TAB'")
""",
                success=False,
            )
            assert "Session with given id not found" in result["stderr"], result
            assert result["code_executed"] is None
            observed = json.loads(
                run(owner, "import json; print(json.dumps(list_tabs()))")["stdout"]
            )
            assert len(observed) == 1 and observed[0]["title"] == "Untouched", observed
            live.connected = True
            live.selected_target = observed[0]["target_id"]
            worker.sessions["live"] = live
            worker.live_bindings["live"] = "fixture"

            def call(data):
                job = worker.call({"session": "live", **data})["job"]
                assert worker.jobs[job]["done"].wait(35)
                return worker.jobs[job]["result"]["response"]

            response = call(
                {"target_id": target, "code": "raise Exception('MODEL CODE RAN')"}
            )
            assert (
                response["phase"] == "target" and response["code_executed"] is False
            ), response
            assert "MODEL CODE RAN" not in response["stderr"]
            assert live.selected_target == observed[0]["target_id"]
            assert response["next_call"] == {"op": "tabs", "session": "live"}
            run(owner, "js(\"document.body.innerText='x'.repeat(1100000)\")")
            response = call({"op": "read", "max_chars": 500})
            assert response["phase"] == "read" and response["code_executed"] is False, (
                response
            )
            assert response["next_call"]["op"] == "exec"
            print(
                "PASS: internal-only connection, no stale-tab replay, pre-execution rejection preserves pin, oversized read recovery"
            )
        finally:
            worker.shutdown()
            if live:
                live.close()
            owner.close()


if __name__ == "__main__":
    main()

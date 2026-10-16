"""Discover Chrome's opt-in debugging endpoint without reading credential stores."""

import json
import os
import re
import sys
from pathlib import Path

SETUP = "In Chrome 144+, open chrome://inspect/#remote-debugging and enable remote debugging. Then call connect and approve Chrome's Allow dialog. Keep Chrome running; never automate that approval."


def browser_roots():
    configured = os.environ.get("GENTS_BROWSER_ROOTS")
    if configured:
        roots = json.loads(configured)
        if not isinstance(roots, dict) or not all(
            isinstance(k, str) and isinstance(v, str) for k, v in roots.items()
        ):
            raise ValueError(
                "GENTS_BROWSER_ROOTS must map browser names to user-data directories"
            )
        return {k: Path(v).expanduser() for k, v in roots.items()}
    home = Path.home()
    if sys.platform == "darwin":
        base = home / "Library/Application Support"
        names = {
            "chrome": "Google/Chrome",
            "chrome-beta": "Google/Chrome Beta",
            "chrome-canary": "Google/Chrome Canary",
            "edge": "Microsoft Edge",
            "brave": "BraveSoftware/Brave-Browser",
            "chromium": "Chromium",
        }
    elif sys.platform == "win32":
        base = Path(os.environ.get("LOCALAPPDATA", home / "AppData/Local"))
        names = {
            "chrome": "Google/Chrome/User Data",
            "edge": "Microsoft/Edge/User Data",
            "brave": "BraveSoftware/Brave-Browser/User Data",
            "chromium": "Chromium/User Data",
        }
    else:
        base = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config"))
        names = {
            "chrome": "google-chrome",
            "chrome-beta": "google-chrome-beta",
            "chromium": "chromium",
            "edge": "microsoft-edge",
            "brave": "BraveSoftware/Brave-Browser",
        }
    return {name: base / path for name, path in names.items()}


def endpoint(root):
    with (root / "DevToolsActivePort").open() as stream:
        lines = stream.read(4096).splitlines()
    if len(lines) < 2 or not lines[0].isdigit() or not 1 <= int(lines[0]) <= 65535:
        raise ValueError("Invalid DevToolsActivePort port")
    if not re.fullmatch(r"/devtools/browser/[A-Za-z0-9_-]+", lines[1]):
        raise ValueError("Invalid DevToolsActivePort browser path")
    return f"ws://127.0.0.1:{int(lines[0])}{lines[1]}"


def discover():
    browsers = []
    for name, root in browser_roots().items():
        row = {"browser": name}
        try:
            endpoint(root)
            row["status"] = "available"
        except FileNotFoundError:
            row["status"] = "setup_required"
        except PermissionError:
            row["status"] = "access_denied"
            row["next"] = (
                "Allow the Gents host application access to browser data in OS privacy settings, then retry discovery."
            )
        except (OSError, ValueError):
            row["status"] = "invalid_endpoint"
        browsers.append(row)
    return {
        "status": "discovered",
        "browsers": browsers,
        "next": "Tell the user Chrome may show an Allow dialog, then call connect with their chosen available browser ID and a task session. Wait for the user to approve in Chrome; do not automate approval."
        if any(row["status"] == "available" for row in browsers)
        else SETUP,
        "scope": "Available means Chrome published an endpoint, not that a connection is approved or still alive. Browser IDs identify installations, not signed-in profiles. Tab-to-profile identity is not exposed here; never infer it.",
    }


def resolve(browser):
    roots = browser_roots()
    if browser not in roots:
        raise ValueError(
            "Unknown browser; call discover and use one of its browser IDs"
        )
    return endpoint(roots[browser])

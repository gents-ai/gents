#!/usr/bin/env python3
import os
import pwd
import shlex
import subprocess
import sys
from pathlib import Path


def runtime_is_running(fleet_home: Path, owner: int, processes: str) -> bool:
    for line in processes.splitlines():
        fields = line.strip().split(maxsplit=1)
        if len(fields) != 2 or not fields[0].isdecimal() or int(fields[0]) != owner:
            continue
        try:
            args = shlex.split(fields[1])
        except ValueError:
            continue
        if len(args) < 4 or Path(args[0]).name != "gents" or args[1] != "server":
            continue
        for index, arg in enumerate(args[2:], start=2):
            if arg == "--home" and index + 1 < len(args):
                if Path(args[index + 1]).resolve() == fleet_home:
                    return True
            elif arg.startswith("--home="):
                if Path(arg.removeprefix("--home=")).resolve() == fleet_home:
                    return True
    return False


def check(account_home: Path, environment: dict[str, str], uid: int, processes: str) -> None:
    fleet = environment.get("GENTS_CI_FLEET_HOME", "")
    if not fleet or not Path(fleet).is_absolute():
        raise ValueError("GENTS_CI_FLEET_HOME must name the live fleet runtime home")
    fleet_home = Path(fleet).resolve(strict=True)
    if not fleet_home.is_dir():
        raise ValueError("GENTS_CI_FLEET_HOME must be an existing directory")
    account_home = account_home.resolve(strict=True)
    process_home = environment.get("HOME", "")
    if not process_home or Path(process_home).resolve(strict=True) != account_home:
        raise ValueError("HOME must match the runner's OS account home")
    if fleet_home == account_home or account_home in fleet_home.parents:
        raise ValueError("fleet runtime home is inside the runner account home")
    owner = fleet_home.stat().st_uid
    if uid == 0 or owner == uid:
        raise ValueError("fleet runtime home is owned by the runner account")
    if not runtime_is_running(fleet_home, owner, processes):
        raise ValueError("configured fleet home does not match a running gents server")
    if os.access(fleet_home, os.R_OK) or os.access(fleet_home, os.X_OK):
        raise ValueError("runner account can access the fleet runtime home")
    if os.path.lexists(account_home / ".gents"):
        raise ValueError("runner account has a persistent .gents home")
    if environment.get("GENTS_HOME"):
        raise ValueError("runner environment must not set GENTS_HOME")


def main() -> int:
    try:
        processes = subprocess.run(
            ["ps", "-axo", "uid=,args="], check=True, capture_output=True, text=True
        ).stdout
        check(Path(pwd.getpwuid(os.getuid()).pw_dir), dict(os.environ), os.getuid(), processes)
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"::error::Isolated runtime runner check failed: {error}", file=sys.stderr)
        return 1
    print("Runtime runner account is isolated from the configured fleet home.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

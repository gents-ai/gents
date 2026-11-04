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
        if len(args) < 3 or Path(args[0]).name != "gents" or args[1] != "server":
            continue
        for index, arg in enumerate(args[2:], start=2):
            if arg == "--home" and index + 1 < len(args):
                if Path(args[index + 1]) == fleet_home:
                    return True
            elif arg.startswith("--home="):
                if Path(arg.removeprefix("--home=")) == fleet_home:
                    return True
    return False


def check(
    account_home: Path, environment: dict[str, str], uid: int, fleet_uid: int, processes: str
) -> None:
    fleet = environment.get("GENTS_CI_FLEET_HOME", "")
    if not fleet or not Path(fleet).is_absolute() or ".." in Path(fleet).parts:
        raise ValueError("GENTS_CI_FLEET_HOME must name the live fleet runtime home")
    fleet_home = Path(fleet)
    account_home_unresolved = account_home.absolute()
    account_home = account_home.resolve(strict=True)
    process_home = environment.get("HOME", "")
    if not process_home or Path(process_home).resolve(strict=True) != account_home:
        raise ValueError("HOME must match the runner's OS account home")
    if any(
        home == fleet_home or home in fleet_home.parents
        for home in (account_home_unresolved, account_home)
    ):
        raise ValueError("fleet runtime home is inside the runner account home")
    if uid == 0 or fleet_uid == uid:
        raise ValueError("fleet runtime uses the runner account")
    if not runtime_is_running(fleet_home, fleet_uid, processes):
        raise ValueError(
            "configured fleet user and home do not match a visible running gents server; "
            "check the configuration and process visibility"
        )
    if os.access(fleet_home, os.R_OK) or os.access(fleet_home, os.X_OK):
        raise ValueError("runner account can access the fleet runtime home")
    if os.path.lexists(account_home / ".gents"):
        raise ValueError("runner account has a persistent .gents home")
    if environment.get("GENTS_HOME"):
        raise ValueError("runner environment must not set GENTS_HOME")


def main() -> int:
    try:
        fleet_user = os.environ.get("GENTS_CI_FLEET_USER", "")
        if not fleet_user:
            raise ValueError("GENTS_CI_FLEET_USER must name the live fleet runtime user")
        processes = subprocess.run(
            ["ps", "-axo", "uid=,args="], check=True, capture_output=True, text=True
        ).stdout
        check(
            Path(pwd.getpwuid(os.getuid()).pw_dir),
            dict(os.environ),
            os.getuid(),
            pwd.getpwnam(fleet_user).pw_uid,
            processes,
        )
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"::error::Isolated runtime runner check failed: {error}", file=sys.stderr)
        return 1
    print("Runtime runner account is isolated from the configured fleet home.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

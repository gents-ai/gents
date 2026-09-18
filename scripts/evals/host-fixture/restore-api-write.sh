#!/bin/sh
set -eu

# Installed host maintenance command; callers cannot select another resource.
[ "$#" -eq 0 ] || { echo 'This command accepts no arguments' >&2; exit 2; }
[ -d /host/api-work ] && [ ! -L /host/api-work ] || exit 3
[ "$(stat -c %u /host/api-work)" = "$(id -u)" ] || exit 3
case "$(stat -c %a /host/api-work)" in
    500) chmod u+w /host/api-work; echo 'Restored owner write permission on /host/api-work' ;;
    700) echo 'Owner write permission is already present; no change' ;;
    *) echo 'Unexpected permissions; refusing to broaden the repair' >&2; exit 3 ;;
esac

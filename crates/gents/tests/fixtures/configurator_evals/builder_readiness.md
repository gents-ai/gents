Verify this new coding workspace by doing the work now. Inside {{USER_HOME}}, create a directory named readiness. Write readiness/test.sh containing exactly:
#!/bin/sh
set -eu
test "$((2 + 2))" -eq 4
printf 'BUILD_TEST_OK\n' > "$(dirname "$0")/result.txt"

Execute the script in a separate command-tool call with command "sh" and args ["readiness/test.sh"], from the workspace root. Do not wrap that execution in a shell program or combine it with file creation: we need the runtime process receipt to identify the script that ran. Read result.txt and report the test outcome. Do not write result.txt yourself; the script must produce it. No network or dependencies are needed.

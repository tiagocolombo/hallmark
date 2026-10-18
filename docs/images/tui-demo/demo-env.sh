# The recording's environment: `. demo-env.sh <root>`. A private HOME and
# HALL_HOME under <root>, no model or GitHub calls, a fictional author, and
# this checkout's `hall` first on the PATH.
DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export DEMO_DIR DEMO_ROOT="$1"
export HOME="$1/home" HALL_HOME="$1/hallhome"
export HALL_CLAUDE=/usr/bin/false HALL_GH=/usr/bin/false HALL_NOTIFY_LOG="$1/notify.log"
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME="Sam Doe" GIT_AUTHOR_EMAIL=sam@example.com GIT_COMMITTER_NAME="Sam Doe" GIT_COMMITTER_EMAIL=sam@example.com
export PATH="$DEMO_DIR/../../../target/debug:$PATH"
unset HALL_RUNNER HALL_NO_HOOKS HALL_HEADLESS HALL_ACTOR CLAUDECODE CLAUDE_SESSION_ID CLAUDE_CODE_SESSION_ID

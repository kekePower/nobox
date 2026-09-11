#!/usr/bin/env bash
set -euo pipefail

desktop=${1:?usage: x11-session-entry.sh DESKTOP SOURCE_DIR}
source_dir=${2:?usage: x11-session-entry.sh DESKTOP SOURCE_DIR}
session_command=$(sed -n 's/^Exec=//p' "$desktop")

# Mageia's /etc/X11/Xsession first looks up a single executable verbatim.
# Only commands containing spaces reach its full-command shell branch.
# Unknown names go to chksession, which silently chooses a fallback desktop.
if ! type -P "$session_command" >/dev/null && [[ $session_command != *' '* ]]; then
    echo "Xsession would select a fallback instead of: $session_command" >&2
    exit 1
fi

test_dir=$(mktemp -d)
trap 'rm -rf -- "$test_dir"' EXIT INT TERM
cat >"$test_dir/session" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exec /bin/sh -c 'exec '"$NOBOX_TEST_SESSION_COMMAND"' "$@"' nobox-session "$@"
EOF
chmod +x "$test_dir/session"
NOBOX_TEST_SESSION_COMMAND="$session_command" \
    bash "$source_dir/tests/x11-smoke.sh" "$test_dir/session"

echo "X11 desktop entry dispatch passed"

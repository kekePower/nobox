#!/usr/bin/env bash
set -euo pipefail

launcher=${1:?usage: launcher.sh /path/to/nobox-launcher}
test_dir=$(mktemp -d)
cleanup() {
    find "$test_dir" -type f -delete 2>/dev/null || true
    find "$test_dir" -depth -type d -empty -delete 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cp "$launcher" "$test_dir/nobox"
chmod 700 "$test_dir/nobox"
for backend in x11 wayland; do
    cat >"$test_dir/nobox-$backend" <<EOF
#!/usr/bin/env bash
printf '%s\\n' '$backend' "\$@"
EOF
    chmod 700 "$test_dir/nobox-$backend"
done

mapfile -t output < <("$test_dir/nobox" doctor)
test "${output[0]}" = x11
test "${output[1]}" = doctor

mapfile -t output < <("$test_dir/nobox" doctor --backend wayland --nested-x11)
test "${output[0]}" = wayland
test "${output[1]}" = doctor
test "${output[2]}" = --backend
test "${output[3]}" = wayland
test "${output[4]}" = --nested-x11

mapfile -t output < <("$test_dir/nobox" --backend=x11 --exit)
test "${output[0]}" = x11
test "${output[1]}" = --backend=x11
test "${output[2]}" = --exit

if "$test_dir/nobox" --backend invalid >/dev/null 2>"$test_dir/error"; then
    echo "launcher accepted an invalid backend" >&2
    exit 1
fi
grep -Fq "unknown backend 'invalid'" "$test_dir/error"

echo "backend launcher selection passed"

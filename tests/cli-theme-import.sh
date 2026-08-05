#!/usr/bin/env bash
set -euo pipefail

nobox_binary=${1:?usage: cli-theme-import.sh /path/to/nobox}
test_dir=$(mktemp -d)
cleanup() {
    rm -rf -- "$test_dir"
}
trap cleanup EXIT INT TERM

mkdir -p "$test_dir/Clearlooks/openbox-3"
printf '%s\n' \
    'border.width: 1' \
    'padding.width: 3' \
    '*.justify: center' \
    'window.*.border.color: #585a5d' \
    '*.title.bg.color: #8CB0DC' \
    'window.inactive.title.bg.color: #E3E2E0' \
    'window.active.label.text.color: white' \
    'window.inactive.label.text.color: #70747d' \
    'window.active.button.*.bg.color: rgb:92/b4/df' \
    'window.active.button.*.image.color: #444' \
    'menu.items.bg.color: grey85' \
    >"$test_dir/Clearlooks/openbox-3/themerc"

"$nobox_binary" import-openbox-theme "$test_dir/Clearlooks" \
    >"$test_dir/generated.toml" 2>"$test_dir/report.log"
"$nobox_binary" --config "$test_dir/generated.toml" check >/dev/null

for expected in \
    'border_width = 1' \
    'title_alignment = "center"' \
    'active_titlebar = "#8cb0dc"' \
    'inactive_titlebar = "#e3e2e0"' \
    'button_glyph = "#444444"'; do
    if ! grep -Fq "$expected" "$test_dir/generated.toml"; then
        echo "Openbox import omitted expected output: $expected" >&2
        exit 1
    fi
done
if ! grep -q 'without a nobox equivalent' "$test_dir/report.log"; then
    echo "Openbox import did not report lossy compatibility" >&2
    exit 1
fi

output="$test_dir/output/config.toml"
"$nobox_binary" import-openbox-theme "$test_dir/Clearlooks" --output "$output" \
    >/dev/null 2>"$test_dir/output-report.log"
"$nobox_binary" --config "$output" check >/dev/null
if "$nobox_binary" import-openbox-theme "$test_dir/Clearlooks" --output "$output" \
    >/dev/null 2>&1; then
    echo "Openbox import replaced an output file without --force" >&2
    exit 1
fi
"$nobox_binary" import-openbox-theme "$test_dir/Clearlooks" --output "$output" --force \
    >/dev/null 2>&1

echo "Openbox theme CLI import and safe output handling passed"

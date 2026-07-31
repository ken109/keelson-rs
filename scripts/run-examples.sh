#!/usr/bin/env bash
# Every example in examples/, run.
#
# `cargo clippy --workspace --all-targets` already proves they compile. This
# proves they *work*: each one asserts its own output, so a change that keeps
# the types happy and breaks the behaviour still fails here. They need no
# services — the ones that touch a database use SQLite in a temporary file.
#
# The list is read from examples/Cargo.toml rather than hard-coded, so a new
# `[[example]]` is covered the moment it is added.
#
# Run from anywhere:  ./scripts/run-examples.sh
set -euo pipefail

cd "$(dirname "$0")/.."

# `tail -n +2` drops the first match, which is the package's own name; every
# later one introduces an `[[example]]`. (A `while read` loop rather than
# `mapfile`, which macOS's bash 3.2 does not have.)
examples=()
while IFS= read -r name; do
    examples+=("$name")
done < <(sed -n 's/^name = "\(.*\)"$/\1/p' examples/Cargo.toml | tail -n +2)

if [ "${#examples[@]}" -eq 0 ]; then
    echo "no [[example]] targets found in examples/Cargo.toml" >&2
    exit 1
fi

fail=0
for name in "${examples[@]}"; do
    printf '=== %s\n' "$name"
    if ! cargo run --quiet -p keelson-examples --example "$name"; then
        printf '!!! FAILED: %s\n' "$name"
        fail=1
    fi
done

exit "$fail"

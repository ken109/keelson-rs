#!/usr/bin/env bash
# Every feature combination the `keelson` facade crate advertises, compiled.
#
# A facade's whole job is that one dependency line works; a feature that does
# not build is therefore a broken promise rather than a broken corner. The list
# below is not the power set (2^17 configurations of no interest) — it is:
#
#   * the empty set, which must still be a crate that compiles;
#   * every feature alone, which is what catches a missing `dep:` or a cfg
#     guarding the wrong thing;
#   * the mapped-type features alone, which is what catches a `dep?/feature`
#     weak reference written as a strong one (it would drag a driver in);
#   * the combinations a real application would actually write;
#   * --all-features, where every cfg is on at once.
#
# Run from anywhere:  ./scripts/feature-matrix.sh
set -euo pipefail

cd "$(dirname "$0")/.."

combos=(
    ""
    "psql"
    "mysql"
    "sqlite"
    "exec"
    "macros"
    "models"
    "factory"
    "tracing"
    "chrono"
    "uuid"
    "decimal"
    "json"
    "sqlx-psql"
    "sqlx-mysql"
    "sqlx-sqlite"
    "psql,mysql,sqlite"
    "sqlx-psql,sqlx-mysql,sqlx-sqlite"
    "exec,psql"
    "models,macros"
    "sqlx-psql,models,factory,macros"
    "sqlx-psql,models,factory,macros,chrono,uuid,decimal,json,tracing"
    "sqlx-sqlite,models,factory,macros,chrono"
    "sqlx-mysql,models,factory,macros,chrono"
)

fail=0
for features in "${combos[@]}"; do
    printf '=== keelson [%s]\n' "${features:-<no features>}"
    if ! cargo check -p keelson --no-default-features --features "$features" --quiet; then
        printf '!!! FAILED: [%s]\n' "${features:-<no features>}"
        fail=1
    fi
done

printf '=== keelson --all-features\n'
cargo check -p keelson --all-features --quiet || fail=1

exit "$fail"

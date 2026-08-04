#!/usr/bin/env bash
#
# Check that the workspace speaks one version, everywhere it has to.
#
#   ./scripts/check-version-consistency.sh            # check the manifest against itself
#   ./scripts/check-version-consistency.sh 0.2.0      # …and that it equals this (a release tag)
#
# The published crates share one version and depend on each other *by* version,
# so a release has to agree in three places at once:
#
#   1. `[workspace.package] version`      — what every crate inherits
#   2. every `[workspace.dependencies]`   — what the packaged manifests carry, and
#      keelson-* entry's `version`          therefore what a consumer resolves
#   3. CHANGELOG.md                       — `docs/publishing.md` makes an entry a
#                                           precondition for cutting a release
#
# Miss (2) for one crate and that crate ships depending on an older sibling —
# which crates.io will happily accept and never let you take back. This runs on
# every push for that reason, not only at release time: the bump lands as an
# ordinary commit, so the mistake is catchable long before a tag exists.
#
# `keelson-sqlcheck` is the deliberate exception: it is `publish = false`, and a
# version beside its path would demand it exist on crates.io. It must stay
# path-only, so an accidental version there is an error too.

set -euo pipefail

cd "$(dirname "$0")/.."

EXPECTED="${1-}"

python3 - "$EXPECTED" <<'PY'
import sys, re, pathlib, tomllib

expected = sys.argv[1] if len(sys.argv) > 1 else ""
problems = []

manifest = tomllib.loads(pathlib.Path("Cargo.toml").read_text())
version = manifest["workspace"]["package"]["version"]

# 1. the tag, if we were given one
if expected and expected != version:
    problems.append(
        f"tag says {expected!r} but [workspace.package] version is {version!r} — "
        f"run `cargo set-version --workspace {expected}` and commit before tagging"
    )

# 2. every intra-workspace dependency
deps = manifest["workspace"].get("dependencies", {})
for name, spec in sorted(deps.items()):
    if not name.startswith("keelson"):
        continue
    if not isinstance(spec, dict):
        problems.append(f"[workspace.dependencies] {name} is not a table")
        continue

    declared = spec.get("version")
    if name == "keelson-sqlcheck":
        # publish = false, dev-dependency only: cargo strips a path-only
        # dev-dependency from a packaged manifest, and a version here would
        # require it on crates.io. See docs/publishing.md.
        if declared is not None:
            problems.append(
                f"{name} must stay path-only (it is publish = false), but declares version {declared!r}"
            )
    elif declared is None:
        problems.append(f"{name} has no version — `cargo package` refuses a path-only dependency")
    elif declared != version:
        problems.append(f"{name} declares version {declared!r}, workspace is {version!r}")

# 3. the changelog
changelog = pathlib.Path("CHANGELOG.md")
if not changelog.exists():
    problems.append("CHANGELOG.md is missing")
elif not re.search(rf"^## \[{re.escape(version)}\]", changelog.read_text(), re.M):
    problems.append(
        f"CHANGELOG.md has no `## [{version}]` section — "
        f"docs/publishing.md makes one a precondition for a release"
    )

if problems:
    print(f"version consistency: FAILED (workspace version {version})\n", file=sys.stderr)
    for p in problems:
        print(f"  - {p}", file=sys.stderr)
    sys.exit(1)

print(f"version consistency: ok — workspace, {sum(1 for n in deps if n.startswith('keelson') and n != 'keelson-sqlcheck')} intra-workspace deps and CHANGELOG all say {version}")
PY

# The lockfile has to already agree, or the published crates resolve differently
# from what CI built. --locked fails rather than rewriting it; `cargo metadata`
# reaches that check without compiling anything.
cargo metadata --locked --format-version 1 --offline >/dev/null 2>&1 \
  || cargo metadata --locked --format-version 1 >/dev/null
echo "lockfile: ok — Cargo.lock is up to date with Cargo.toml"

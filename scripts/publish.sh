#!/usr/bin/env bash
# Publish every workspace crate to crates.io in dependency order.
#
# The order is derived from `cargo metadata` at run time, never from a
# hand-maintained list: v0.1.0 and v0.3.0 both shipped a stale order
# (facade-after-client, mcp-before-client) and burned ~30 min of retry
# backoff before failing. Dev-dependency edges count too: `cargo publish`
# resolves dev-deps against the registry regardless of --no-verify, which is
# why oxibrain-client may not dev-depend on oxibrain-mcp (publish cycle).
#
# Usage:
#   scripts/publish.sh               # publish (requires CARGO_REGISTRY_TOKEN)
#   scripts/publish.sh --order-only  # print the computed order and exit

set -euo pipefail
cd "$(dirname "$0")/.."

# Crates published with --no-verify: cargo's verify step rebuilds the crate
# standalone in a temp dir, and llama-cpp-2 costs ~10 min per crate there.
# The `cargo build --workspace` + `cargo test --workspace` gates in the
# publish workflow already compile and test these exact sources.
NO_VERIFY="oxibrain-embed-local oxibrain-llm-local"

order() {
  python3 - <<'PY'
import json
import subprocess
import sys

meta = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"]
    )
)

names = {p["name"]: p for p in meta["packages"]}
deps = {n: set() for n in names}
for pkg in meta["packages"]:
    for dep in pkg["dependencies"]:
        # Workspace-internal edges only: path dependencies. Version-only
        # requirements resolve against the registry, not the workspace.
        if dep.get("path") and dep["name"] in names:
            deps[pkg["name"]].add(dep["name"])

# Kahn's algorithm; alphabetical among ready crates for determinism.
order = []
ready = sorted(n for n, ds in deps.items() if not ds)
remaining = {n: set(ds) for n, ds in deps.items() if ds}
while ready:
    n = ready.pop(0)
    order.append(n)
    still = {}
    for m, ds in remaining.items():
        ds.discard(n)
        if ds:
            still[m] = ds
        else:
            ready.append(m)
    ready.sort()
    remaining = still
if remaining:
    sys.exit(
        "workspace dependency cycle — publish is impossible until it is "
        "broken (dev-dependencies count): " + ", ".join(sorted(remaining))
    )
for n in order:
    print(n, names[n]["version"])
PY
}

if [[ "${1:-}" == "--order-only" ]]; then
  order
  exit 0
fi

publish() {
  local crate="$1" version="$2"
  shift 2
  local flags=("$@")
  local out attempt
  for attempt in 1 2 3; do
    echo "::group::Publish $crate@$version (attempt $attempt)"
    if out=$(cargo publish "${flags[@]}" -p "$crate" 2>&1); then
      echo "$out"
      echo "::endgroup::"
      echo "✅ $crate@$version"
      return 0
    fi
    echo "$out"
    echo "::endgroup::"
    # Idempotent re-run: the exact version we are publishing already exists.
    if grep -qF "crate ${crate}@${version} already exists" <<<"$out"; then
      echo "⏭️ $crate@$version (already published)"
      return 0
    fi
    # Deterministic failures — retrying cannot help. A duplicate at a
    # DIFFERENT version means the checkout is not bumped: v0.3.0 was once
    # dispatched from unbumped main, skipped everything at 0.2.0, and the
    # run went green while publishing nothing.
    if grep -qE \
      "already exists on crates.io index|failed to select a version|no matching package named" \
      <<<"$out"; then
      echo "❌ $crate@$version: deterministic failure (see group above)"
      return 1
    fi
    # Transient (network / rate limit): back off and retry.
    sleep $((attempt * 60))
  done
  echo "❌ $crate@$version failed after 3 attempts"
  return 1
}

while read -r crate version; do
  flags=()
  if [[ " $NO_VERIFY " == *" $crate "* ]]; then
    flags+=(--no-verify)
  fi
  publish "$crate" "$version" "${flags[@]}"
done < <(order)

#!/usr/bin/env bash
# Compile the CLI with a fresh consumer lockfile before publishing any crate.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target
dir=$(mktemp -d "$PWD/target/consumer-preflight.XXXXXX")
trap 'rm -rf "$dir"' EXIT
python3 - "$PWD" "$dir" <<'PY'
import json
from pathlib import Path
import sys

root, dest = map(Path, sys.argv[1:])
cli = json.dumps(str(root / "crates/oxibrain-cli"))
(dest / "Cargo.toml").write_text(
    '[package]\nname = "oxibrain_consumer_preflight"\n'
    'version = "0.0.0"\nedition = "2024"\n[workspace]\n'
    '[dependencies]\noxibrain-cli = { path = ' + cli + ' }\n'
    '[lib]\npath = "lib.rs"\n'
)
(dest / "lib.rs").write_text("")
PY
cargo check --manifest-path "$dir/Cargo.toml" --target-dir "$PWD/target"

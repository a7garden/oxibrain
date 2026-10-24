#!/usr/bin/env bash
# Compile CLI integration tests with only the CLI's packaged file inventory.
set -euo pipefail
cd "$(dirname "$0")/.."
package_root="$PWD"
mkdir -p target
package_check_dir=$(mktemp -d "$PWD/target/cli-package-check.XXXXXX")
trap 'rm -rf "$package_check_dir"' EXIT
cargo metadata --locked --no-deps --format-version 1 > "$package_check_dir/metadata.json"
cargo package --list --allow-dirty --no-verify -p oxibrain-cli > "$package_check_dir/files.txt"
python3 - "$package_root" "$package_check_dir" <<'PY'
import json
from pathlib import Path
import shutil
import subprocess
import sys

root, temporary = map(Path, sys.argv[1:])
export = temporary / "source"
export.mkdir()
metadata = json.loads((temporary / "metadata.json").read_text())
cli = root / "crates/oxibrain-cli"
inventory = set((temporary / "files.txt").read_text().splitlines())
generated = {".cargo_vcs_info.json", "Cargo.lock", "Cargo.toml.orig"}

for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md", "LICENSE"):
    source = root / name
    if source.is_file():
        shutil.copy2(source, export / name)

members = set(metadata["workspace_members"])
for package in metadata["packages"]:
    if package["id"] not in members:
        continue
    source = Path(package["manifest_path"]).parent
    relative = source.relative_to(root)
    destination = export / relative
    if source == cli:
        files = sorted(inventory - generated)
    else:
        listed = subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", str(relative)],
            cwd=root,
        ).split(b"\0")
        files = [str(Path(item.decode()).relative_to(relative)) for item in listed if item]
    for name in files:
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        original = source / name
        if not original.is_file():
            # Cargo relocates inherited readme/license files into the package.
            for field in ("readme", "license_file"):
                configured = package.get(field)
                if configured and name == Path(configured).name:
                    original = (source / configured).resolve()
                    break
        if not original.is_file() or not original.resolve().is_relative_to(root):
            sys.exit(f"cannot materialize packaged file {relative / name}")
        shutil.copy2(original, target)

assert not (export / "eval").exists(), "repository-only evaluation files must be absent"
for version in ("v1", "v2", "v3", "v4"):
    name = f"tests/fixtures/longmemeval/{version}-manifest.json"
    if name not in inventory:
        sys.exit(f"CLI package omits {name}")
print("CLI package inventory exported without repository evaluation files")
PY
cargo test --locked --manifest-path "$package_check_dir/source/Cargo.toml" \
  --target-dir "$package_root/target" -p oxibrain-cli --test external_recall

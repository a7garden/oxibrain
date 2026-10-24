#!/usr/bin/env bash
# Verify the macOS release archives and checksum files expected by oxios.
set -euo pipefail

asset_dir="${1:-.}"

verify_archive() {
  local archive="$1" expected_member="$2"
  local checksum="${archive}.sha256"
  local expected_checksum actual_checksum

  [[ -f "$asset_dir/$archive" ]] || {
    echo "missing release archive: $asset_dir/$archive" >&2
    return 1
  }
  [[ -f "$asset_dir/$checksum" ]] || {
    echo "missing release checksum: $asset_dir/$checksum" >&2
    return 1
  }

  expected_checksum="$(cd "$asset_dir" && shasum -a 256 "$archive")"
  actual_checksum="$(cat "$asset_dir/$checksum")"
  [[ "$actual_checksum" == "$expected_checksum" ]] || {
    echo "checksum mismatch or unexpected checksum format: $checksum" >&2
    return 1
  }

  python3 - "$asset_dir/$archive" "$expected_member" <<'PY'
import sys
import tarfile

archive_path, expected_member = sys.argv[1:]
try:
    with tarfile.open(archive_path, "r:gz") as archive:
        members = archive.getmembers()
except (OSError, tarfile.TarError) as error:
    sys.exit(f"invalid archive {archive_path}: {error}")

if len(members) != 1:
    sys.exit(f"{archive_path} must contain exactly one member; found {len(members)}")
member = members[0]
if member.name != expected_member:
    sys.exit(f"{archive_path} must contain top-level '{expected_member}'; found '{member.name}'")
if not member.isfile():
    sys.exit(f"{archive_path}:{member.name} must be a regular file")
if member.size <= 0:
    sys.exit(f"{archive_path}:{member.name} must be nonempty")
if member.mode & 0o111 == 0:
    sys.exit(f"{archive_path}:{member.name} must be executable")
PY
}

verify_archive "oxibrain-aarch64-apple-darwin.tar.gz" "oxibrain"
verify_archive "oxibrain-mlx-aarch64-apple-darwin.tar.gz" "oxibrain-mlx"
echo "release archives and checksums verified"

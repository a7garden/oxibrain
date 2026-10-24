#!/usr/bin/env bash
# Offline regression checks for release archive and checksum validation.
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

python3 - "$tmp" "$PWD/scripts/verify-release-assets.sh" <<'PY'
import io
import pathlib
import subprocess
import sys
import tarfile

root = pathlib.Path(sys.argv[1])
verifier = pathlib.Path(sys.argv[2])


def write_archive(path, entries):
    with tarfile.open(path, "w:gz") as archive:
        for name, kind, mode, payload in entries:
            member = tarfile.TarInfo(name)
            member.mode = mode
            if kind == "file":
                member.size = len(payload)
                archive.addfile(member, io.BytesIO(payload))
            elif kind == "symlink":
                member.type = tarfile.SYMTYPE
                member.linkname = payload.decode()
                archive.addfile(member)
            else:
                raise AssertionError(f"unknown fixture type: {kind}")


def refresh_checksums():
    for archive in (
        "oxibrain-aarch64-apple-darwin.tar.gz",
        "oxibrain-mlx-aarch64-apple-darwin.tar.gz",
    ):
        with (root / f"{archive}.sha256").open("wb") as checksum:
            subprocess.run(
                ["shasum", "-a", "256", archive],
                cwd=root,
                stdout=checksum,
                check=True,
            )


def write_pair(standard_entries):
    write_archive(
        root / "oxibrain-aarch64-apple-darwin.tar.gz", standard_entries
    )
    write_archive(
        root / "oxibrain-mlx-aarch64-apple-darwin.tar.gz",
        [("oxibrain-mlx", "file", 0o755, b"mlx binary fixture")],
    )
    refresh_checksums()


def verify(expected, description):
    result = subprocess.run(
        ["bash", str(verifier), str(root)], capture_output=True, text=True
    )
    if expected and result.returncode != 0:
        raise AssertionError(f"{description} should pass: {result.stderr}")
    if not expected and result.returncode == 0:
        raise AssertionError(f"{description} unexpectedly passed")


write_pair([("oxibrain", "file", 0o755, b"standard binary fixture")])
verify(True, "valid executable archives")

# A modified archive with the original checksum must fail before tar inspection.
with (root / "oxibrain-aarch64-apple-darwin.tar.gz").open("ab") as archive:
    archive.write(b"tampered")
verify(False, "checksum mismatch")

invalid_cases = [
    ([("oxibrain", "symlink", 0o777, b"target")], "symlink member"),
    ([("oxibrain", "file", 0o755, b"")], "empty member"),
    ([("oxibrain", "file", 0o644, b"binary")], "non-executable member"),
    ([("nested/oxibrain", "file", 0o755, b"binary")], "nested member"),
    (
        [
            ("oxibrain", "file", 0o755, b"first"),
            ("oxibrain", "file", 0o755, b"second"),
        ],
        "duplicate members",
    ),
]
for entries, description in invalid_cases:
    write_pair(entries)
    verify(False, description)

print("release asset verifier tests passed")
PY

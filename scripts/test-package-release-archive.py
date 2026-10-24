#!/usr/bin/env python3
"""Offline tests for deterministic release archive packaging."""

from __future__ import annotations

import os
import stat
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


def test_archive(helper: Path) -> None:
    payload = b"release-binary-fixture\x00\x01\x02"
    epoch = "1792800000"

    with tempfile.TemporaryDirectory(prefix="oxibrain-release-package-test-") as temp:
        root = Path(temp)
        source = root / "oxibrain"
        source.write_bytes(payload)
        source.chmod(0o755)
        first = root / "first.tar.gz"
        second = root / "second.tar.gz"

        def run(output: Path) -> None:
            env = os.environ.copy()
            env["SOURCE_DATE_EPOCH"] = epoch
            subprocess.run(
                [
                    sys.executable,
                    str(helper),
                    "--source",
                    str(source),
                    "--member",
                    "oxibrain",
                    "--output",
                    str(output),
                ],
                check=True,
                env=env,
            )

        os.utime(source, (1_700_000_000, 1_700_000_000))
        run(first)
        source.chmod(0o600)
        os.utime(source, (1_600_000_000, 1_600_000_000))
        run(second)

        if first.read_bytes() != second.read_bytes():
            raise AssertionError("archive bytes changed with source mtime or permissions")
        if first.read_bytes()[4:8] != b"\x00\x00\x00\x00":
            raise AssertionError("gzip header contains a nonzero timestamp")

        with tarfile.open(first, "r:gz") as archive:
            members = archive.getmembers()
            if len(members) != 1:
                raise AssertionError(f"expected exactly one archive member, got {len(members)}")
            member = members[0]
            if member.name != "oxibrain" or not member.isfile():
                raise AssertionError("archive member is not the expected regular file")
            if member.size != len(payload) or not member.mode & stat.S_IXUSR:
                raise AssertionError("archive member is empty, truncated, or not executable")
            if member.uid != 0 or member.gid != 0 or member.uname or member.gname:
                raise AssertionError("archive member owner metadata is not normalized")
            if member.mode != 0o755 or member.mtime != int(epoch):
                raise AssertionError("archive member mode or tagged-commit timestamp is wrong")
            extracted = archive.extractfile(member)
            if extracted is None or extracted.read() != payload:
                raise AssertionError("archive member content differs from the source binary")


def test_publish_order(publish_script: Path) -> None:
    cargo_mock = """#!/bin/sh
set -eu
if [ "${1:-}" = metadata ]; then
  count_file="$MOCK_STATE/count"
  count=0
  if [ -f "$count_file" ]; then count=$(cat "$count_file"); fi
  count=$((count + 1))
  printf '%s\n' "$count" >"$count_file"
  if [ "$count" = 1 ]; then
    printf '%s\n' '{"packages":[{"name":"fixture","version":"1.2.3","dependencies":[]}]}'
  elif [ "$MOCK_ORDER_MODE" = cycle ]; then
    printf '%s\n' '{"packages":[{"name":"first","version":"1.2.3","dependencies":[{"name":"second","path":"../second"}]},{"name":"second","version":"1.2.3","dependencies":[{"name":"first","path":"../first"}]}]}'
  elif [ "$MOCK_ORDER_MODE" = empty ]; then
    printf '%s\n' '{"packages":[]}'
  else
    exit 9
  fi
elif [ "${1:-}" = publish ]; then
  printf '%s\n' "$*" >>"$MOCK_STATE/published"
else
  echo "unexpected cargo invocation: $*" >&2
  exit 10
fi
"""

    for mode in ("cycle", "empty", "metadata_failure"):
        with tempfile.TemporaryDirectory(prefix=f"oxibrain-publish-{mode}-test-") as temp:
            root = Path(temp)
            repo = root / "repo"
            scripts = repo / "scripts"
            bin_dir = root / "bin"
            state = root / "state"
            scripts.mkdir(parents=True)
            bin_dir.mkdir()
            state.mkdir()
            fixture_script = scripts / "publish.sh"
            fixture_script.write_bytes(publish_script.read_bytes())
            cargo_path = bin_dir / "cargo"
            cargo_path.write_text(cargo_mock)
            cargo_path.chmod(0o755)

            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Release test"], check=True)
            subprocess.run(
                ["git", "-C", str(repo), "config", "user.email", "release-test@example.invalid"],
                check=True,
            )
            subprocess.run(["git", "-C", str(repo), "add", "scripts/publish.sh"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "test fixture"], check=True)
            subprocess.run(["git", "-C", str(repo), "tag", "v1.2.3"], check=True)

            env = os.environ.copy()
            env.update(
                {
                    "PATH": f"{bin_dir}{os.pathsep}{env['PATH']}",
                    "RELEASE_TAG": "v1.2.3",
                    "MOCK_ORDER_MODE": mode,
                    "MOCK_STATE": str(state),
                }
            )
            result = subprocess.run(
                ["bash", str(fixture_script)],
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
            )
            if result.returncode == 0:
                raise AssertionError(f"{mode} metadata unexpectedly allowed publication to succeed")
            expected_error = {
                "cycle": "workspace dependency cycle",
                "empty": "workspace dependency order is empty",
                "metadata_failure": "returned non-zero exit status 9",
            }[mode]
            if expected_error not in result.stdout:
                raise AssertionError(
                    f"{mode} fixture failed before the expected order error: {result.stdout}"
                )
            published = state / "published"
            if published.exists() and published.stat().st_size:
                raise AssertionError(f"{mode} metadata failure invoked cargo publish: {published.read_text()}")


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    test_archive(repo / "scripts/package-release-archive.py")
    test_publish_order(repo / "scripts/publish.sh")
    print("release packaging and publish-order tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

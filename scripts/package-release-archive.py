#!/usr/bin/env python3
"""Create a reproducible gzip-compressed tar archive for a release binary."""

from __future__ import annotations

import argparse
import gzip
import os
import stat
import sys
import tarfile
from pathlib import Path


def package(source: Path, member_name: str, output: Path, epoch: int) -> None:
    if not member_name or Path(member_name).name != member_name or member_name in {".", ".."}:
        raise ValueError("archive member must be a single top-level filename")
    if epoch < 0:
        raise ValueError("SOURCE_DATE_EPOCH must be a non-negative integer")

    source_stat = source.stat()
    if not stat.S_ISREG(source_stat.st_mode):
        raise ValueError(f"release binary is not a regular file: {source}")
    if source_stat.st_size == 0:
        raise ValueError(f"release binary is empty: {source}")

    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw_output:
        with gzip.GzipFile(fileobj=raw_output, mode="wb", filename="", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                info = tarfile.TarInfo(name=member_name)
                info.size = source_stat.st_size
                info.mtime = epoch
                info.mode = 0o755
                info.uid = 0
                info.gid = 0
                info.uname = ""
                info.gname = ""
                info.type = tarfile.REGTYPE
                with source.open("rb") as binary:
                    archive.addfile(info, binary)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--member", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    try:
        epoch = int(os.environ["SOURCE_DATE_EPOCH"])
        package(args.source, args.member, args.output, epoch)
    except KeyError:
        print("SOURCE_DATE_EPOCH is required", file=sys.stderr)
        return 2
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

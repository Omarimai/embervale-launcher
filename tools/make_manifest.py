#!/usr/bin/env python3
"""Build a manifest.json from a directory of release files.

The manifest is the contract between a release and the launcher, so the tool
that writes it lives with the launcher rather than with whatever produced the
files. Hand it the exported game and the URL the files will be served from:

    python tools/make_manifest.py \
        --dir dist/0.4.1 \
        --base-url https://cdn.embervale.example/0.4.1/ \
        --version 0.4.1 \
        --launch Embervale.exe \
        --out dist/0.4.1/manifest.json

Every file under --dir is included except the manifest itself and dotfiles, so
what the launcher installs is exactly what was uploaded -- no list to keep in
step. Dotfiles are skipped because build tooling leaves them in output
directories -- Godot's .gdignore, macOS's .DS_Store -- and a player has no use
for any of them.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

CHUNK = 1024 * 1024


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", required=True, type=Path,
                        help="directory of files to publish")
    parser.add_argument("--base-url", required=True,
                        help="URL the directory will be served at")
    parser.add_argument("--version", required=True,
                        help="release name shown in the launcher")
    parser.add_argument("--launch", required=True,
                        help="executable PLAY starts, relative to --dir")
    parser.add_argument("--news", type=Path,
                        help="optional JSON array of {title, date, body}")
    parser.add_argument("--out", required=True, type=Path,
                        help="where to write manifest.json")
    args = parser.parse_args()

    root: Path = args.dir
    if not root.is_dir():
        print(f"{root} is not a directory", file=sys.stderr)
        return 1

    launch = root / args.launch
    if not launch.is_file():
        # Caught here rather than by a player whose PLAY button does nothing.
        print(f"--launch {args.launch!r} does not exist in {root}", file=sys.stderr)
        return 1

    base = args.base_url if args.base_url.endswith("/") else args.base_url + "/"
    out = args.out.resolve()

    files = []
    for path in sorted(root.rglob("*")):
        if not path.is_file() or path.resolve() == out:
            continue
        rel_parts = path.relative_to(root).parts
        if any(part.startswith(".") for part in rel_parts):
            continue
        rel = path.relative_to(root).as_posix()
        files.append({
            "path": rel,
            "sha256": sha256(path),
            "size": path.stat().st_size,
            "url": base + rel,
        })

    if not files:
        print(f"no files found under {root}", file=sys.stderr)
        return 1

    manifest = {
        "version": args.version,
        "launch": Path(args.launch).as_posix(),
        "files": files,
    }

    if args.news:
        manifest["news"] = json.loads(args.news.read_text(encoding="utf-8"))

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    total = sum(f["size"] for f in files)
    print(f"wrote {args.out}: {len(files)} files, {total / 1024 / 1024:.1f} MB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

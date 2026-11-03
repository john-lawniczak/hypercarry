#!/usr/bin/env python3
"""Check an export candidate without reading ignored local credentials or data.

This is a narrow publication guard, not a comprehensive secret scanner. Default
mode checks Git candidates after maintainer exclusions. --root checks an actual
filtered export, including unexpected untracked files, without using its history.
"""

import argparse
import fnmatch
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
from urllib.parse import unquote, urlsplit


EXCLUDE = ("Z-*.md", "TODO.md", "tools/export-public.sh")
SECRET_PATTERNS = (
    re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----"),
    re.compile(rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
    re.compile(rb"\bgh[pousr]_[A-Za-z0-9]{36,}\b"),
    re.compile(rb"\bgithub_pat_[A-Za-z0-9_]{50,}\b"),
)
LINK = re.compile(r"!?\[[^\]\n]*\]\(([^\s)]+)(?:\s+[^)]*)?\)")


def excluded(name):
    return any(fnmatch.fnmatchcase(name, pattern) for pattern in EXCLUDE)


def forbidden(name):
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name:
        return True
    if any(part in {".aws", ".ssh", "target", "data", "__pycache__"} for part in path.parts):
        return True
    leaf = path.name.lower()
    return (
        (leaf.startswith(".env") and not leaf.endswith(".example"))
        or leaf in {"credentials", "id_rsa", "id_ed25519", "id_ecdsa"}
        or path.suffix.lower() in {".pem", ".p12", ".pfx", ".key", ".pyc", ".parquet", ".duckdb"}
    )


def check(root, names):
    """Return path-only findings; never print matching credential contents."""
    names = set(names)
    errors = []
    directories = {str(parent) for name in names for parent in PurePosixPath(name).parents}
    for name in sorted(names):
        if excluded(name) or forbidden(name):
            errors.append(f"{name}: forbidden export path")
            continue
        file = root / name
        if file.is_symlink() or any(parent.is_symlink() for parent in file.parents if parent != root):
            errors.append(f"{name}: symbolic links are not exportable")
            continue
        if not file.is_file():
            errors.append(f"{name}: missing or nonregular file")
            continue
        if file.stat().st_size > 10 * 1024 * 1024:
            errors.append(f"{name}: large artifact requires explicit publication review")
            continue
        contents = file.read_bytes()
        if any(pattern.search(contents) for pattern in SECRET_PATTERNS):
            errors.append(f"{name}: possible credential material; review privately")
        if file.suffix.lower() != ".md":
            continue
        # Only inline links are checked; code blocks and remote links are not.
        markdown = re.sub(r"(?ms)^```.*?^```[^\n]*", "", contents.decode("utf-8"))
        for link in LINK.findall(markdown):
            parsed = urlsplit(link.strip("<>"))
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            candidate = file.parent / unquote(parsed.path)
            try:
                relative = candidate.resolve().relative_to(root).as_posix()
            except ValueError:
                errors.append(f"{name}: local link escapes export")
                continue
            if relative not in names and relative not in directories:
                errors.append(f"{name}: missing exported link target {relative}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, help="actual filtered tree, rather than Git candidates")
    args = parser.parse_args()
    if args.root:
        root = args.root.resolve(strict=True)
        names = [item.relative_to(root).as_posix() for item in root.rglob("*")
                 if ".git" not in item.relative_to(root).parts and (item.is_symlink() or not item.is_dir())]
    else:
        root = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()).resolve()
        candidates = subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root
        ).decode().split("\0")
        names = [name for name in candidates if name and not excluded(name)]
    errors = check(root, names)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Public export checks passed for {len(set(names))} files; independent secret/content review still required.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

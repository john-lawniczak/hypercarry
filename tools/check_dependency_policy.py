#!/usr/bin/env python3
"""Enforce the bounded chrono maintenance exception; never waive vulnerabilities."""
from datetime import date
import json
from pathlib import Path
import subprocess
import sys

EXPIRES = date(2027, 1, 31)
ALLOWED_PARENTS = {("arrow-array", "60.0.0"), ("parquet", "60.0.0")}


def check(metadata, parents, signing_dependencies, today):
    packages = {p["id"]: p for p in metadata["packages"]}
    chrono = {key for key, p in packages.items() if p["name"] == "chrono"}
    if not chrono:
        return
    if today > EXPIRES:
        raise ValueError("chrono exception expired; review upstream storage migration")
    if any(packages[key]["version"] != "0.4.45" for key in chrono):
        raise ValueError("chrono version differs from assessed dependency")
    if not parents or not parents <= ALLOWED_PARENTS:
        raise ValueError("unassessed chrono parents: " + str(parents))
    for member in metadata["workspace_members"]:
        if any(d["name"] == "chrono" for d in packages[member]["dependencies"]):
            raise ValueError("workspace code must not depend directly on chrono")
    if "chrono" in signing_dependencies:
        raise ValueError("chrono must not enter the signing crate graph")


def tree_packages(args, root):
    import re
    output = subprocess.check_output(["cargo", "tree", "--locked", "--all-features",
        "--target", "all", "--prefix", "none", *args], cwd=root, text=True)
    return set(re.findall(r"^([a-zA-Z0-9_-]+) v([^\s]+)", output, re.M))


def main():
    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked",
        "--all-features", "--format-version", "1"], cwd=root, text=True))
    parents = tree_packages(["--workspace", "--invert", "chrono", "--depth", "1"], root) - {("chrono", "0.4.45")}
    signing = {name for name, _ in tree_packages(["-p", "hypercarry-signing"], root)}
    check(metadata, parents, signing, date.today())
    print("Dependency scope passed: chrono storage-only exception expires " + EXPIRES.isoformat())


if __name__ == "__main__":
    try: main()
    except (ValueError, subprocess.CalledProcessError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)

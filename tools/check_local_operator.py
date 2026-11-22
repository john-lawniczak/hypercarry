#!/usr/bin/env python3
"""Build integration binaries and run offline local-operator tests. No live keys."""
import argparse
import importlib.metadata
import json
from pathlib import Path
import subprocess
import sys
import unittest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, help="optional private verification receipt")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    if Path.cwd().resolve() != root:
        raise ValueError("run from the repository root")
    sys.path.insert(0, str(root))
    version = importlib.metadata.version("hyperliquid-python-sdk")
    if version != "0.24.0":
        raise ValueError("use hyperliquid-python-sdk==0.24.0")
    subprocess.run(["cargo", "build", "--locked", "-p", "hypercarry-cli", "-p", "hypercarry-testnet-operator",
                    "--bin", "hypercarry", "--bin", "hypercarry-bracket-journal"], check=True)
    suite = unittest.defaultTestLoader.discover("tools/local_operator", top_level_dir=".")
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    passed = result.wasSuccessful() and not result.skipped
    from tools.local_operator.entry import artifact
    from tools.local_operator.signer import now_ms
    from tools.local_operator.store import write
    receipt = {"schema_version": 1, "observed_at_ms": now_ms(), "offline_tests_passed": passed,
               "tests_run": result.testsRun, "skipped": len(result.skipped), "sdk_version": version,
               "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
               "working_tree_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], text=True)),
               "binaries": [artifact(root / "target/debug/hypercarry"), artifact(root / "target/debug/hypercarry-bracket-journal")],
               "credentialed_testnet_verified": False, "independent_review_complete": False, "mainnet_authorized": False}
    if args.report:
        write(args.report, receipt, replace=True)
    print(json.dumps(receipt, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())

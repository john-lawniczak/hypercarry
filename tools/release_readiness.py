#!/usr/bin/env python3
"""Freeze a Linux candidate and check supplied review evidence; never approve trading."""
import argparse
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
import re
import shutil

from operator_files import json_bytes, parse_json, read_file, require, sha256, write_new
import release_bundle

CONFIGS = {"executor", "runtime", "services", "supervisor", "kms", "operations"}
SCENARIOS = ("security_review", "kms_access", "kms_signing_faults", "transport_partition", "executor_loss",
             "private_stream_disconnect", "partial_fill_recovery", "alert_delivery", "rollback_drill",
             "testnet_session_1", "testnet_session_2", "testnet_session_3")


def named_paths(values, allowed):
    paths = {}
    for value in values:
        name, separator, path = value.partition("=")
        require(separator and name in allowed and name not in paths and path, "invalid or duplicate named path")
        paths[name] = Path(path)
    return paths


def configurations(paths):
    require(set(paths) == CONFIGS, "supply all six configuration files: " + ", ".join(sorted(CONFIGS)))
    snapshots = {name: read_file(path) for name, path in paths.items()}
    for data in snapshots.values():
        require(isinstance(parse_json(data), dict), "configuration must be a JSON object")
    return snapshots


def validate_release(manifest):
    require(manifest["target"] in {"x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"}, "candidate requires a Linux GNU release bundle")
    files = {item["path"]: item for item in manifest["files"]}
    for program in release_bundle.REQUIRED_PROGRAMS:
        require(files.get("bin/" + program, {}).get("mode") == 0o755, "missing required executable: " + program)
    for units in release_bundle.ROLE_UNITS.values():
        for unit in units:
            require("deploy/mainnet/" + unit in files, "missing required service unit: " + unit)


def prepare(bundle, expected, configs, output):
    snapshots = configurations(configs)
    stream, archive, release, _ = release_bundle.verified(bundle, expected)
    try:
        validate_release(release)
        manifest = {"schema_version": 1, "bundle_sha256": expected, "source_commit": release["source_commit"],
                    "target": release["target"], "cargo_lock_sha256": release["cargo_lock_sha256"],
                    "configuration_sha256": {name: sha256(data) for name, data in snapshots.items()},
                    "required_scenarios": list(SCENARIOS), "trading_authorized": False}
        manifest_bytes = json_bytes(manifest)
        output.mkdir(mode=0o700)
        try:
            (output / "config").mkdir(mode=0o700)
            for name, data in snapshots.items():
                write_new(output / "config" / (name + ".json"), data)
            stream.seek(0)
            with (output / "release.tar.gz").open("xb") as target:
                # Directory is private; chmod the file explicitly for later copies.
                (output / "release.tar.gz").chmod(0o600)
                shutil.copyfileobj(stream, target, 1024 * 1024)
            write_new(output / "candidate.json", manifest_bytes)
            write_new(output / "pending.json", json_bytes({"candidate_sha256": sha256(manifest_bytes),
                      "status": "pending", "missing": list(SCENARIOS), "trading_authorized": False}))
        except BaseException:
            shutil.rmtree(output)
            raise
    finally:
        archive.close()
        stream.close()
    return {"candidate_sha256": sha256(manifest_bytes), "source_commit": release["source_commit"],
            "missing": list(SCENARIOS), "trading_authorized": False}


def validate_record(path, scenario, candidate_digest):
    raw = read_file(path)
    record = parse_json(raw)
    fields = {"schema_version", "candidate_sha256", "scenario", "outcome", "operator", "reviewer", "performed_at",
              "observations", "artifacts", "session_id"}
    require(set(record) == fields and record["schema_version"] == 1, "invalid evidence record fields/schema")
    require(record["candidate_sha256"] == candidate_digest and record["scenario"] == scenario, "record candidate/scenario mismatch")
    require(record["outcome"] in {"passed", "failed"}, "evidence must record the actual passed/failed outcome")
    for key in ("operator", "reviewer", "observations"):
        require(isinstance(record[key], str) and 0 < len(record[key].strip()) <= 16000, "missing or oversized " + key)
    require(record["operator"].strip().casefold() != record["reviewer"].strip().casefold(), "independent reviewer must differ from operator")
    require(isinstance(record["performed_at"], str) and record["performed_at"].endswith("Z"), "evidence time must be UTC with Z suffix")
    performed = datetime.fromisoformat(record["performed_at"].replace("Z", "+00:00"))
    require(performed <= datetime.now(timezone.utc), "evidence cannot be dated in the future")
    if scenario.startswith("testnet_session_"):
        require(isinstance(record["session_id"], str) and 0 < len(record["session_id"].strip()) <= 200, "testnet session identity required")
    else:
        require(record["session_id"] is None, "non-session record must have null session_id")
    require(isinstance(record["artifacts"], list) and 1 <= len(record["artifacts"]) <= 32, "require 1..32 retained artifacts")
    names = set()
    for item in record["artifacts"]:
        require(set(item) == {"path", "sha256"}, "invalid artifact record")
        name = item["path"]
        require(isinstance(name, str) and name, "missing artifact path")
        relative = PurePosixPath(name)
        require(not relative.is_absolute() and ".." not in relative.parts and str(relative) == name
                and "\\" not in name and name not in names, "unsafe or duplicate artifact path")
        names.add(name)
        artifact = path.parent / name
        # Evidence paths are relative to a trusted operator-owned directory.
        require(not any((path.parent / Path(*relative.parts[:n])).is_symlink() for n in range(1, len(relative.parts) + 1)), "artifact symlink refused")
        require(sha256(read_file(artifact)) == item["sha256"], "artifact digest mismatch")
    return record, sha256(raw)


def verify(candidate, expected, configs, records):
    require(re.fullmatch(r"[0-9a-f]{64}", expected), "require trusted candidate SHA-256")
    raw = read_file(candidate / "candidate.json")
    require(sha256(raw) == expected, "candidate manifest digest mismatch")
    manifest = parse_json(raw)
    require(set(manifest) == {"schema_version", "bundle_sha256", "source_commit", "target", "cargo_lock_sha256",
                             "configuration_sha256", "required_scenarios", "trading_authorized"}
            and manifest["schema_version"] == 1 and manifest["trading_authorized"] is False
            and manifest["required_scenarios"] == list(SCENARIOS), "invalid candidate manifest")
    current = configurations(configs)
    require(set(manifest["configuration_sha256"]) == CONFIGS, "incomplete candidate configuration")
    for name, digest in manifest["configuration_sha256"].items():
        require(sha256(read_file(candidate / "config" / (name + ".json"))) == digest, "frozen configuration changed")
        require(sha256(current[name]) == digest, "installed configuration differs from candidate: " + name)
    stream, archive, release, _ = release_bundle.verified(candidate / "release.tar.gz", manifest["bundle_sha256"])
    try:
        validate_release(release)
        for key in ("source_commit", "target", "cargo_lock_sha256"):
            require(release[key] == manifest[key], "candidate release identity mismatch")
    finally:
        archive.close()
        stream.close()
    require(set(records) <= set(SCENARIOS), "unknown evidence scenario")
    results, sessions = {}, set()
    for scenario, path in records.items():
        record, digest = validate_record(path, scenario, expected)
        if scenario.startswith("testnet_session_"):
            session = record["session_id"].strip()
            require(session not in sessions, "testnet sessions must have distinct identities")
            sessions.add(session)
        results[scenario] = {"outcome": record["outcome"], "record_sha256": digest}
    missing = sorted(set(SCENARIOS) - set(results))
    failed = sorted(name for name, item in results.items() if item["outcome"] != "passed")
    return {"schema_version": 1, "candidate_sha256": expected, "evidence_complete_for_review": not missing and not failed,
            "missing": missing, "failed": failed, "records": results, "trading_authorized": False,
            "limitations": "Checks integrity/binding of supplied claims, not their truth or reviewer independence. Existing release gates still apply."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    prepare_parser = sub.add_parser("prepare")
    prepare_parser.add_argument("--bundle", type=Path, required=True)
    prepare_parser.add_argument("--sha256", required=True)
    prepare_parser.add_argument("--output", type=Path, required=True)
    verify_parser = sub.add_parser("verify")
    verify_parser.add_argument("--candidate", type=Path, required=True)
    verify_parser.add_argument("--sha256", required=True)
    verify_parser.add_argument("--record", action="append", default=[], metavar="SCENARIO=PATH")
    verify_parser.add_argument("--report", type=Path)
    for item in (prepare_parser, verify_parser):
        item.add_argument("--config", action="append", required=True, metavar="NAME=PATH")
    args = parser.parse_args()
    configs = named_paths(args.config, CONFIGS)
    if args.command == "prepare":
        result = prepare(args.bundle, args.sha256, configs, args.output)
    else:
        result = verify(args.candidate, args.sha256, configs, named_paths(args.record, SCENARIOS))
        if args.report:
            write_new(args.report, json_bytes(result))
    print(json_bytes(result).decode(), end="")
    if args.command == "verify" and not result["evidence_complete_for_review"]:
        raise SystemExit(2)


if __name__ == "__main__":
    main()

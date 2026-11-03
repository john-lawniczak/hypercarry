#!/usr/bin/env python3
"""Prepare an offline AWS change-set input from explicit inventory snapshots."""
import argparse
from pathlib import Path
import re
import shutil

from operator_files import json_bytes, parse_json, read_file, require, sha256, write_new

TEMPLATE = Path(__file__).resolve().parents[1] / "deploy/mainnet/aws-reference.json"
PARAMETERS = {"VpcId", "ExecutorSubnetId", "RecoverySubnetId", "ImageId", "ImageOwnerId", "InstanceType"}


def prepare(settings_path, subnets_path, images_path, output):
    snapshots = {"settings.json": read_file(settings_path), "subnets.json": read_file(subnets_path),
                 "images.json": read_file(images_path), "template.json": read_file(TEMPLATE)}
    settings, subnets, images = (parse_json(snapshots[name]) for name in ("settings.json", "subnets.json", "images.json"))
    require(set(settings) == PARAMETERS, "settings must specify every template parameter, with no extra fields")
    require(all(isinstance(value, str) for value in settings.values()), "parameters must be strings")
    for key, prefix in [("VpcId", "vpc"), ("ExecutorSubnetId", "subnet"), ("RecoverySubnetId", "subnet"), ("ImageId", "ami")]:
        require(re.fullmatch(prefix + r"-[0-9a-f]+", settings[key]), "invalid " + key)
    require(re.fullmatch(r"[0-9]{12}", settings["ImageOwnerId"]), "invalid image owner")
    require(settings["InstanceType"] in {"m6i.large", "m6i.xlarge"}, "unsupported reference instance type")
    selected = []
    for role in ("Executor", "Recovery"):
        matches = [item for item in subnets["Subnets"] if item["SubnetId"] == settings[role + "SubnetId"]]
        require(len(matches) == 1, "subnet missing or duplicated")
        subnet = matches[0]
        require(subnet["VpcId"] == settings["VpcId"] and subnet["State"] == "available", "subnet unavailable or wrong VPC")
        require(subnet.get("MapPublicIpOnLaunch") is False, "require private subnets without automatic public IPs")
        require(isinstance(subnet.get("AvailabilityZoneId"), str) and subnet["AvailabilityZoneId"], "missing AZ identity")
        selected.append(subnet)
    require(selected[0]["AvailabilityZoneId"] != selected[1]["AvailabilityZoneId"], "recovery must be in a separate availability zone")
    matches = [item for item in images["Images"] if item["ImageId"] == settings["ImageId"]]
    require(len(matches) == 1, "AMI missing or duplicated")
    image = matches[0]
    require(image.get("OwnerId") == settings["ImageOwnerId"], "AMI owner mismatch")
    require(image.get("State") == "available" and image.get("Architecture") == "x86_64"
            and image.get("VirtualizationType") == "hvm" and image.get("RootDeviceType") == "ebs"
            and image.get("RootDeviceName") == "/dev/sda1" and image.get("PlatformDetails") == "Linux/UNIX",
            "AMI must be available x86_64 HVM Linux/UNIX with an EBS /dev/sda1 root")
    parameters = [{"ParameterKey": key, "ParameterValue": settings[key]} for key in sorted(settings)]
    snapshots["parameters.json"] = json_bytes(parameters)
    plan = {"schema_version": 1, "deployment_performed": False, "trading_authorized": False,
            "target": "x86_64-unknown-linux-gnu", "availability_zone_ids": [item["AvailabilityZoneId"] for item in selected],
            "sha256": {name: sha256(data) for name, data in snapshots.items()},
            "not_checked": ["inventory authenticity/freshness", "AMI contents and SSM agent", "routes/NAT/endpoints/DNS", "IAM effective permissions",
                            "protected transport", "state mounts", "monitoring receipts", "independent security review"]}
    # No replacement of an earlier reviewed plan; no AWS calls or credentials.
    output.mkdir(mode=0o700)
    try:
        for name, data in snapshots.items():
            write_new(output / name, data)
        write_new(output / "plan.json", json_bytes(plan))
    except BaseException:
        shutil.rmtree(output)
        raise
    return plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("settings", "subnets", "images", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    print(json_bytes(prepare(args.settings, args.subnets, args.images, args.output)).decode(), end="")


if __name__ == "__main__":
    main()

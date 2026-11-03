import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import prepare_aws as aws
from operator_files import parse_json, read_file


class AwsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.settings = {"VpcId": "vpc-123", "ExecutorSubnetId": "subnet-1", "RecoverySubnetId": "subnet-2",
                         "ImageId": "ami-123", "ImageOwnerId": "123456789012", "InstanceType": "m6i.large"}
        self.subnets = {"Subnets": [{"SubnetId": "subnet-" + str(n), "VpcId": "vpc-123", "State": "available",
                                    "MapPublicIpOnLaunch": False, "AvailabilityZoneId": "use1-az" + str(n)} for n in (1, 2)]}
        self.images = {"Images": [{"ImageId": "ami-123", "OwnerId": "123456789012", "State": "available", "Architecture": "x86_64",
                                   "VirtualizationType": "hvm", "RootDeviceType": "ebs", "RootDeviceName": "/dev/sda1", "PlatformDetails": "Linux/UNIX"}]}

    def run_prepare(self):
        paths = [self.root / name for name in ("settings", "subnets", "images")]
        for path, value in zip(paths, [self.settings, self.subnets, self.images]):
            path.write_text(json.dumps(value))
        return aws.prepare(*paths, self.root / "plan")

    def test_plan_snapshots_validated_inputs_without_deploying_or_overwriting(self):
        plan = self.run_prepare()
        self.assertFalse(plan["deployment_performed"])
        self.assertFalse(plan["trading_authorized"])
        for name, digest in plan["sha256"].items():
            self.assertEqual(aws.sha256((self.root / "plan" / name).read_bytes()), digest)
        with self.assertRaises(FileExistsError):
            self.run_prepare()

    def test_rejects_shared_fault_domain_public_subnets_and_incompatible_ami(self):
        mutations = [("subnet", "AvailabilityZoneId", "use1-az1"), ("subnet", "MapPublicIpOnLaunch", True),
                     ("subnet", "VpcId", "vpc-456"), ("image", "Architecture", "arm64"),
                     ("image", "State", "pending"), ("image", "OwnerId", "000000000000"),
                     ("image", "RootDeviceName", "/dev/xvda"), ("image", "PlatformDetails", "Windows")]
        for kind, key, value in mutations:
            with self.subTest(kind=kind, key=key):
                original_subnets, original_images = copy.deepcopy(self.subnets), copy.deepcopy(self.images)
                target = self.subnets["Subnets"][1] if kind == "subnet" else self.images["Images"][0]
                target[key] = value
                with self.assertRaises(ValueError):
                    self.run_prepare()
                self.assertFalse((self.root / "plan").exists())
                self.subnets, self.images = original_subnets, original_images

    def test_reference_template_preserves_custody_and_state_boundaries(self):
        resources = json.loads(aws.TEMPLATE.read_text())["Resources"]
        key = resources["SigningKey"]
        self.assertEqual(key["Properties"]["KeySpec"], "ECC_SECG_P256K1")
        self.assertEqual(key["DeletionPolicy"], "Retain")
        allow = resources["RecoveryKeyAccess"]["Properties"]["PolicyDocument"]["Statement"][0]
        self.assertEqual(allow["Resource"], {"Fn::GetAtt": ["SigningKey", "Arn"]})
        deny = resources["ExecutorSigningDeny"]["Properties"]["PolicyDocument"]["Statement"][0]
        self.assertEqual(deny, {"Effect": "Deny", "Action": ["kms:Sign"], "Resource": "*"})
        for role in ("Executor", "Recovery"):
            launch = resources[role + "Launch"]["Properties"]["LaunchTemplateData"]
            self.assertEqual(launch["MetadataOptions"]["HttpTokens"], "required")
            self.assertFalse(launch["NetworkInterfaces"][0]["AssociatePublicIpAddress"])
            self.assertNotIn("UserData", launch)
            self.assertTrue(launch["BlockDeviceMappings"][0]["Ebs"]["Encrypted"])
            self.assertEqual(resources[role + "State"]["UpdateReplacePolicy"], "Retain")
            self.assertTrue(resources[role + "State"]["Properties"]["Encrypted"])
            self.assertNotIn("SecurityGroupIngress", resources[role + "Group"]["Properties"])

    def test_untrusted_json_and_nonregular_inputs_are_rejected(self):
        for data in (b'{"x":1,"x":2}', b'{"x":NaN}'):
            with self.assertRaises(ValueError):
                parse_json(data)
        regular = self.root / "regular"
        regular.write_bytes(b'{}')
        link = self.root / "link"
        link.symlink_to(regular)
        with self.assertRaises(OSError):
            read_file(link)
        with self.assertRaises(ValueError):
            read_file(self.root)

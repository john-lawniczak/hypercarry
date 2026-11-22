import copy
from datetime import date
import unittest

from tools.check_dependency_policy import ALLOWED_PARENTS, check


class DependencyPolicyTests(unittest.TestCase):
    def test_exception_is_bounded_by_version_scope_and_expiry(self):
        metadata = {"packages": [{"id": "chrono", "name": "chrono", "version": "0.4.45"},
                                {"id": "workspace", "name": "workspace", "dependencies": []}],
                    "workspace_members": ["workspace"]}
        today = date(2026, 11, 22)
        check(metadata, ALLOWED_PARENTS, {"time"}, today)
        for parents, signing, day in [(ALLOWED_PARENTS | {("new-parent", "1")}, set(), today),
                                     (set(), set(), today), (ALLOWED_PARENTS, {"chrono"}, today),
                                     (ALLOWED_PARENTS, set(), date(2027, 2, 1))]:
            with self.assertRaises(ValueError): check(metadata, parents, signing, day)
        changed = copy.deepcopy(metadata)
        changed["packages"][0]["version"] = "0.4.46"
        with self.assertRaises(ValueError): check(changed, ALLOWED_PARENTS, set(), today)
        changed = copy.deepcopy(metadata)
        changed["packages"][1]["dependencies"] = [{"name": "chrono"}]
        with self.assertRaises(ValueError): check(changed, ALLOWED_PARENTS, set(), today)
        check({"packages": [], "workspace_members": []}, set(), set(), date(2028, 1, 1))

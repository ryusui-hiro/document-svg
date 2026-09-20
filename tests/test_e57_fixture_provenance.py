import hashlib
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class E57FixtureProvenanceTests(unittest.TestCase):
    def test_public_bunny_fixture_matches_its_source_hash(self):
        relative = "tests/fixtures/sample_e57_bunny.e57"
        fixture = ROOT / relative
        manifest = json.loads((ROOT / "tests/fixtures/e57_bunny.provenance.json").read_text())
        self.assertEqual(
            hashlib.sha256(fixture.read_bytes()).hexdigest(),
            manifest["sha256"][relative],
        )
        self.assertIn("e57-3d-imgfmt.sourceforge.net/data.html", manifest["documentation"])
        self.assertIn("Test Data License", manifest["license"])


if __name__ == "__main__":
    unittest.main()

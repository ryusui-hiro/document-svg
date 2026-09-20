import hashlib
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    'dicom_pdf_fixture_generator', ROOT / 'scripts/generate-dicom-pdf-fixture.py'
)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class DicomPdfFixtureTests(unittest.TestCase):
    def test_fixture_is_deterministic_and_matches_its_manifest(self):
        fixture = (ROOT / 'tests/fixtures/sample_encapsulated_pdf.dcm').read_bytes()
        generated = generator.dicom_encapsulated_pdf(generator.synthetic_pdf())
        self.assertEqual(generated, fixture)
        manifest = json.loads((ROOT / 'tests/fixtures/dicom_pdf.provenance.json').read_text())
        self.assertEqual(
            hashlib.sha256(fixture).hexdigest(),
            manifest['sha256']['tests/fixtures/sample_encapsulated_pdf.dcm'],
        )


if __name__ == '__main__':
    unittest.main()

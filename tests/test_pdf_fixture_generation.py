import hashlib
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    'pdf_fixture_generator', ROOT / 'scripts/generate-pdf-jpx-alpha-fixture.py'
)
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)
preblend_spec = importlib.util.spec_from_file_location(
    'jpx_preblended_fixture_generator', ROOT / 'scripts/generate-jpx-preblended-fixture.py'
)
preblend_generator = importlib.util.module_from_spec(preblend_spec)
preblend_spec.loader.exec_module(preblend_generator)
browser_spec = importlib.util.spec_from_file_location(
    'browser_smoke_fixture_generator', ROOT / 'scripts/generate-browser-smoke-pdf.py'
)
browser_generator = importlib.util.module_from_spec(browser_spec)
browser_spec.loader.exec_module(browser_generator)


class PdfFixtureGenerationTests(unittest.TestCase):
    def test_browser_smoke_pdf_fixture_is_deterministic_and_hash_verified(self):
        fixture = (ROOT / 'tests/fixtures/sample_browser.pdf').read_bytes()
        manifest = json.loads((ROOT / 'tests/fixtures/browser_smoke.provenance.json').read_text())
        self.assertEqual(browser_generator.build_pdf(), fixture)
        self.assertEqual(
            hashlib.sha256(fixture).hexdigest(),
            manifest['sha256']['tests/fixtures/sample_browser.pdf'],
        )

    def test_preblend_generator_uses_the_jpx_matte_formula(self):
        source = (ROOT / 'tests/fixtures/sample_jpeg2000_rgba.pam').read_bytes()
        result = preblend_generator.preblend_pam(source)
        samples = result.split(b'ENDHDR\n', 1)[1]
        self.assertEqual(
            samples[:16],
            bytes([255, 0, 0, 255, 0, 128, 127, 128, 0, 0, 255, 0, 64, 64, 255, 64]),
        )

    def test_jpx_pdf_fixtures_are_deterministic_and_hash_verified(self):
        source = (ROOT / 'tests/fixtures/sample_jpeg2000_rgba.jp2').read_bytes()
        fixture = (ROOT / 'tests/fixtures/sample_jpx_alpha.pdf').read_bytes()
        soft_mask_source = (ROOT / 'tests/fixtures/sample_jpeg2000_rgb.jp2').read_bytes()
        soft_mask_fixture = (ROOT / 'tests/fixtures/sample_jpx_soft_mask.pdf').read_bytes()
        preblended_source = (ROOT / 'tests/fixtures/sample_jpeg2000_rgba_preblended.jp2').read_bytes()
        preblended_fixture = (ROOT / 'tests/fixtures/sample_jpx_smask_in_data_2.pdf').read_bytes()
        manifest = json.loads((ROOT / 'tests/fixtures/pdf_images.provenance.json').read_text())

        generated = generator.build_pdf(source)
        generated_soft_mask = generator.build_external_soft_mask_pdf(soft_mask_source)
        generated_preblended = generator.build_preblended_jpx_pdf(preblended_source)

        self.assertEqual(generated, fixture)
        self.assertEqual(generated_soft_mask, soft_mask_fixture)
        self.assertEqual(generated_preblended, preblended_fixture)
        self.assertEqual(
            hashlib.sha256(fixture).hexdigest(),
            manifest['sha256']['tests/fixtures/sample_jpx_alpha.pdf'],
        )
        self.assertEqual(
            hashlib.sha256(soft_mask_fixture).hexdigest(),
            manifest['sha256']['tests/fixtures/sample_jpx_soft_mask.pdf'],
        )
        self.assertEqual(
            hashlib.sha256(preblended_fixture).hexdigest(),
            manifest['sha256']['tests/fixtures/sample_jpx_smask_in_data_2.pdf'],
        )


if __name__ == '__main__':
    unittest.main()

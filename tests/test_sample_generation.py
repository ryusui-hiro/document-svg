import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('sample_generator', ROOT / 'scripts/make_samples.py')
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)


class SampleGenerationTests(unittest.TestCase):
    def test_published_sources_are_generated_and_hash_verified(self):
        manifest = json.loads((ROOT / 'samples/provenance.json').read_text())
        self.assertEqual(manifest['license'], 'MIT OR Apache-2.0')
        with tempfile.TemporaryDirectory() as directory:
            for name, builder in generator.BUILDERS.items():
                with self.subTest(name=name):
                    output = Path(directory) / name
                    builder(output)
                    published = (ROOT / 'samples/source' / name).read_bytes()
                    self.assertEqual(output.read_bytes(), published)
                    self.assertEqual(hashlib.sha256(published).hexdigest(), manifest['sha256'][f'source/{name}'])
                    fixture = {
                        'sample-parcels.dbf': 'sample_attributes.dbf',
                        'sample.toml': 'sample_config.toml',
                        'sample.yaml': 'sample_config.yaml',
                        'sample-config.xml': 'sample_config.xml',
                        'sample.properties': 'sample_properties.properties',
                        'sample.bpmn': 'sample_bpmn.bpmn',
                        'sample.dmn': 'sample_dmn.dmn',
                        'sample.cmmn': 'sample_cmmn.cmmn',
                        'sample.reqif': 'sample_requirements.reqif',
                        'sample.xmi': 'sample_model.xmi',
                    }.get(name)
                    if fixture:
                        self.assertEqual(output.read_bytes(), (ROOT / 'tests/fixtures' / fixture).read_bytes())


if __name__ == '__main__':
    unittest.main()

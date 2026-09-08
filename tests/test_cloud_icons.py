"""Offline regression tests: python3 -m unittest discover -s tests -p 'test_cloud_icons.py'."""
import base64
import copy
import contextlib
import io
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

SPEC = importlib.util.spec_from_file_location('cloud_icons', Path(__file__).resolve().parents[1] / 'authoring/cloud_icons.py')
cloud = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cloud)
ICON = b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 20"><defs><style>.st0{fill:red}</style></defs><path id="same" class="st0" d="M0 0h40v20H0z"/></svg>'


class CloudIconsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.cache = Path(self.temporary.name)
        self.source = dict(key='aws', provider='aws', kind='mixed', release='test', source_page='https://aws.amazon.com/architecture/icons/')
        self.archive = self.cache / 'icons.zip'
        with zipfile.ZipFile(self.archive, 'w') as bundle:
            bundle.writestr('Architecture-Service-Icons_test/Arch_Compute/16/Arch_AWS-Lambda_16.svg', ICON)
            bundle.writestr('Architecture-Service-Icons_test/Arch_Compute/64/Arch_AWS-Lambda_64.svg', ICON)
        records, rejected = cloud.index_archive(self.source, self.archive, self.cache)
        self.assertEqual(rejected, [])
        self.catalog = dict(schema_version=1, verified_on='test', icons=records)
        self.diagram = dict(schema_version=1, title='Cloud & <SVG>', nodes=[
            dict(id='a', icon='aws/service/aws-lambda', row=0, column=0, label='Lambda A'),
            dict(id='b', icon='aws/service/aws-lambda', row=0, column=1, label='Lambda B'),
        ], edges=[{'from': 'a', 'to': 'b', 'label': 'HTTPS'}])

    def test_deduplicates_sizes_and_preserves_original_bytes(self):
        self.assertEqual(len(self.catalog['icons']), 1)
        item, uri = cloud.resolve(self.catalog, self.cache, 'aws/service/aws-lambda')
        self.assertEqual(item['size'], 64)
        self.assertEqual(len(item['variants']), 2)
        self.assertEqual(base64.b64decode(uri.split(',')[1]), ICON)

    def test_search_provider_and_exact_id(self):
        self.assertEqual(cloud.search(self.catalog, 'lambda', 'aws')[0]['id'], 'aws/service/aws-lambda')
        self.assertEqual(cloud.search(self.catalog, 'aws/service/aws-lambda')[0]['size'], 64)
        self.assertEqual(cloud.search(self.catalog, 'lambda', 'azure'), [])
        self.assertEqual(cloud.search(self.catalog, 'missing'), [])

    def test_resolve_fails_for_missing_and_tampered_icon(self):
        with self.assertRaisesRegex(ValueError, 'unknown'):
            cloud.resolve(self.catalog, self.cache, 'aws/service/made-up')
        (self.cache / self.catalog['icons'][0]['path']).write_bytes(ICON + b' ')
        with self.assertRaisesRegex(ValueError, 'checksum'):
            cloud.resolve(self.catalog, self.cache, 'aws/service/aws-lambda')

    def test_resolve_rejects_path_escape(self):
        self.catalog['icons'][0]['path'] = '../escape.svg'
        with self.assertRaisesRegex(ValueError, 'escapes'):
            cloud.resolve(self.catalog, self.cache, 'aws/service/aws-lambda')

    def test_renderer_is_deterministic_and_isolates_ids_and_styles(self):
        first, report = cloud.render_diagram(self.diagram, self.catalog, self.cache)
        second, _ = cloud.render_diagram(self.diagram, self.catalog, self.cache)
        self.assertEqual(first, second)
        root = ET.fromstring(first)
        images = root.findall(f'.//{{{cloud.SVG_NS}}}image')
        self.assertEqual(len(images), 2)
        for image in images:
            self.assertEqual(image.get('preserveAspectRatio'), 'xMidYMid meet')
            self.assertEqual(base64.b64decode(image.get('href').split(',')[1]), ICON)
        self.assertNotIn('id="same"', first)
        self.assertIn('Cloud &amp; &lt;SVG&gt;', first)
        self.assertEqual(report['node_count'], 2)
        self.assertEqual(report['warnings'], [])

    def test_renderer_rejects_invalid_structure(self):
        cases = []
        for key, value in [('row', -1), ('row', True), ('column', 8), ('icon', 'missing'), ('label', '長' * 100)]:
            spec = copy.deepcopy(self.diagram)
            spec['nodes'][0][key] = value
            cases.append(spec)
        for mutation in ['duplicate_id', 'overlap', 'unknown_edge', 'diagonal', 'unknown_field']:
            spec = copy.deepcopy(self.diagram)
            if mutation == 'duplicate_id': spec['nodes'][1]['id'] = 'a'
            if mutation == 'overlap': spec['nodes'][1]['column'] = 0
            if mutation == 'unknown_edge': spec['edges'][0]['to'] = 'missing'
            if mutation == 'diagonal': spec['nodes'][1]['row'] = 1
            if mutation == 'unknown_field': spec['nodes'][0]['rotate'] = 90
            cases.append(spec)
        for spec in cases:
            with self.subTest(spec=spec), self.assertRaises(ValueError):
                cloud.render_diagram(spec, self.catalog, self.cache)

    def test_renderer_rejects_edge_through_another_node(self):
        self.diagram['nodes'].append(dict(id='c', icon='aws/service/aws-lambda', row=0, column=2))
        self.diagram['edges'] = [{'from': 'a', 'to': 'c'}]
        with self.assertRaisesRegex(ValueError, 'crosses node b'):
            cloud.render_diagram(self.diagram, self.catalog, self.cache)

    def test_category_use_emits_warning(self):
        item = self.catalog['icons'][0]
        item['kind'] = 'category'
        _, report = cloud.render_diagram(self.diagram, self.catalog, self.cache)
        self.assertEqual(len(report['warnings']), 1)

    def test_rejects_active_external_or_malformed_svg(self):
        for content in [b'<script/>', b'<image href="file:///secret"/>', b'<g onload="x"/>',
                        b'<style>.x{fill:url(https://invalid)}</style>', b'<style>@import "x";</style>',
                        b'<style>.x{fill:u\\72l(x)}</style>', b'<use href="https://invalid"/>']:
            with self.subTest(content=content), self.assertRaises(ValueError):
                cloud.validate_svg(b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">' + content + b'</svg>')
        for data in [b'<!DOCTYPE svg><svg/>', b'<svg/>', b'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 NaN 10"/>']:
            with self.subTest(data=data), self.assertRaises(ValueError):
                cloud.validate_svg(data)

    def test_zip_traversal_is_rejected_without_extracting(self):
        with zipfile.ZipFile(self.archive, 'w') as bundle:
            bundle.writestr('../escaped.svg', ICON)
        with self.assertRaisesRegex(ValueError, 'unsafe ZIP'):
            cloud.index_archive(self.source, self.archive, self.cache)
        self.assertFalse((self.cache.parent / 'escaped.svg').exists())

    def test_unsupported_icons_are_reported(self):
        with zipfile.ZipFile(self.archive, 'a') as bundle:
            bundle.writestr('bad.svg', b'<svg/>')
        records, rejected = cloud.index_archive(self.source, self.archive, self.cache)
        self.assertEqual(len(records), 1)
        self.assertEqual(rejected[0]['source_path'], 'bad.svg')

    def test_google_core_and_category_ids_are_distinct(self):
        source = dict(provider='gcp', kind='service')
        self.assertEqual(cloud.identity(source, 'Unique Icons/Cloud Run/SVG/CloudRun-512-color.svg')[0], 'gcp/service/cloud-run')
        source['kind'] = 'category'
        self.assertEqual(cloud.identity(source, 'Category Icons/Compute/SVG/Compute.svg')[0], 'gcp/category/compute')

    def test_gallery_and_render_cli_refuse_overwrite(self):
        output = self.cache / 'gallery.html'
        cloud.gallery(self.catalog, self.cache, output)
        self.assertIn('data-search=', output.read_text())
        with self.assertRaises(FileExistsError):
            cloud.gallery(self.catalog, self.cache, output)
        cloud.atomic_write(self.cache / 'catalog.json', cloud.json_bytes(self.catalog))
        source = self.cache / 'diagram.json'
        source.write_text(json.dumps(self.diagram))
        svg = self.cache / 'diagram.svg'
        args = ['--cache', str(self.cache), 'render', str(source), '--output', str(svg)]
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(cloud.main(args), 0)
            before = svg.read_bytes()
            self.assertEqual(cloud.main(args), 1)
        self.assertEqual(svg.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()

import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'plugins/document-svg/skills/document-svg/scripts/render-preview.py'
spec = importlib.util.spec_from_file_location('preview_renderer', SCRIPT)
preview = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview)


class PreviewSecurityTests(unittest.TestCase):
    def test_safe_svg(self):
        preview.validate_svg(b'<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(#g)"/></svg>')

    def test_rejects_external_or_active_svg(self):
        for body in [
            '<image href="file:///etc/passwd"/>',
            '<image href="//example.com/a.png"/>',
            '<image href="../a.png"/>',
            '<image href="https&#58;//example.com/a.png"/>',
            '<x:script xmlns:x="http://www.w3.org/2000/svg"/>',
            '<rect style="fill:u\\72l(https://example.com/a)"/>',
            '<style>@import "https://example.com/a";</style>',
        ]:
            with self.subTest(body=body), self.assertRaises(ValueError):
                preview.validate_svg(('<svg>' + body + '</svg>').encode())


if __name__ == '__main__':
    unittest.main()

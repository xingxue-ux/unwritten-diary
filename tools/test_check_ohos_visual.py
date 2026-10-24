import shutil
from pathlib import Path
import tempfile
import unittest

import check_ohos_visual as visual

ROOT = Path(__file__).resolve().parents[1]


class VisualParityTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for folder in (visual.ETS, visual.RES / 'base/media', visual.RES / 'rawfile/fonts',
                       visual.FLUTTER, Path('apps/diary_app/assets/fonts')):
            source = ROOT / folder
            shutil.copytree(source, self.root / folder, dirs_exist_ok=True)

    def change(self, path: Path, before: str, after: str) -> None:
        target = self.root / path
        source = target.read_text(encoding='utf-8')
        self.assertIn(before, source)
        target.write_text(source.replace(before, after, 1), encoding='utf-8')

    def test_current_sources_pass(self) -> None:
        self.assertEqual(visual.check(self.root), [])

    def test_color_drift_fails(self) -> None:
        self.change(visual.ETS / 'ui/Sketch.ets', "'#F8F7F2'", "'#FFFFFF'")
        self.assertTrue(any('palette.canvas' in error for error in visual.check(self.root)))

    def test_missing_font_fails(self) -> None:
        (self.root / visual.RES / 'rawfile/fonts/DiaryDisplay-AppSubset.ttf').unlink()
        self.assertTrue(any('font:' in error for error in visual.check(self.root)))

    def test_font_must_be_identical(self) -> None:
        (self.root / visual.RES / 'rawfile/fonts/DiaryDisplay-AppSubset.ttf').write_bytes(b'wrong font')
        self.assertTrue(any('font:' in error for error in visual.check(self.root)))

    def test_new_ui_character_requires_display_glyph(self) -> None:
        self.change(visual.ETS / 'pages/RecordPage.ets', '此刻的记录', '此刻的齉录')
        self.assertTrue(any('font coverage:' in error for error in visual.check(self.root)))

    def test_license_is_required(self) -> None:
        (self.root / visual.RES / 'rawfile/fonts/OFL.txt').unlink()
        self.assertTrue(any('OFL.txt' in error for error in visual.check(self.root)))

    def test_unstyled_control_fails(self) -> None:
        self.change(visual.ETS / 'pages/RecordPage.ets', '.attributeModifier(sketchButton)', '')
        self.assertTrue(any('buttons: RecordPage' in error for error in visual.check(self.root)))

    def test_system_palette_fails(self) -> None:
        self.change(visual.ETS / 'pages/RecordPage.ets', 'SketchColors.paper', "$r('sys.color.ohos_id_color_card_bg')")
        self.assertTrue(any('theme: system' in error for error in visual.check(self.root)))

    def test_breakpoint_drift_fails(self) -> None:
        self.change(visual.ETS / 'pages/Index.ets', 'Number(area.width) >= 850', 'Number(area.width) >= 900')
        self.assertTrue(any('layout:' in error for error in visual.check(self.root)))

    def test_placeholder_route_fails(self) -> None:
        self.change(visual.ETS / 'pages/Index.ets', 'StylesPage()', "this.PlaceholderPage('风格', '')")
        self.assertTrue(any('routes:' in error for error in visual.check(self.root)))

    def test_comment_is_not_a_control(self) -> None:
        target = self.root / visual.ETS / 'pages/RecordPage.ets'
        with target.open('a', encoding='utf-8') as handle:
            handle.write("\n// Button('example').backgroundColor($r('sys.color.example'))\n")
        self.assertEqual(visual.check(self.root), [])

    def test_percentage_overlay_regression_fails(self) -> None:
        self.change(visual.ETS / 'ui/Sketch.ets', 'extends DrawModifier', 'extends OldOverlay')
        self.assertTrue(any('outline:' in error for error in visual.check(self.root)))

    def test_draw_modifier_inside_attribute_wrapper_fails(self) -> None:
        self.change(visual.ETS / 'ui/Sketch.ets',
                    '.labelStyle({ maxLines: 3, overflow: TextOverflow.Ellipsis });',
                    '.labelStyle({ maxLines: 3, overflow: TextOverflow.Ellipsis }).drawModifier(sketchOutline());')
        self.assertTrue(any('API 23' in error for error in visual.check(self.root)))

    def test_low_contrast_foreground_fails(self) -> None:
        self.change(visual.ETS / 'pages/JobsPage.ets',
                    '.fontColor(SketchColors.ink)', '.fontColor(SketchColors.button)')
        self.assertTrue(any('contrast:' in error for error in visual.check(self.root)))


if __name__ == '__main__':
    unittest.main()

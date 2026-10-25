import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import stage_ohos_models as stage


class ModelStagingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.destination = self.root / 'out'
        self.asr, self.bge = self.root / 'asr', self.root / 'bge'
        manifests = self.root / 'models/manifest'
        manifests.mkdir(parents=True)
        for name in ('sense-voice-LICENSE-MIT.txt', 'bge-small-zh-v1.5-LICENSE-MIT.txt', 'ohos-local-models-NOTICE.md'):
            (manifests / name).write_text('fixture notice', encoding='utf-8')
        for _, name, key in stage.GROUPS:
            source = self.asr if key == 'asr' else self.bge
            source.mkdir()
            files = []
            for filename in ('model.onnx', 'tokens.txt'):
                data = (key + filename).encode()
                (source / filename).write_bytes(data)
                files.append({'path': filename, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()})
            (manifests / name).write_text(json.dumps({'files': files}), encoding='utf-8')

    def run_stage(self):
        return stage.stage(self.asr, self.bge, self.destination, self.root)

    def test_stage_and_check(self):
        result = self.run_stage()
        self.assertEqual(len(result['files']), 4)
        self.assertEqual(stage.check(self.destination, self.root), result)

    def test_bad_source_rejected_before_copy(self):
        path = self.bge / 'model.onnx'
        path.write_bytes(b'x' * path.stat().st_size)
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            self.run_stage()
        self.assertFalse(self.destination.exists())

    def test_tampered_output_rejected(self):
        result = self.run_stage()
        (self.destination / result['files'][0]['path']).write_bytes(b'bad')
        with self.assertRaises(ValueError):
            stage.check(self.destination, self.root)

    def test_missing_license_rejected(self):
        self.run_stage()
        (self.destination / 'SenseVoice-LICENSE-MIT.txt').unlink()
        with self.assertRaisesRegex(ValueError, 'notice'):
            stage.check(self.destination, self.root)

    def test_extra_model_rejected(self):
        self.run_stage()
        (self.destination / 'unwanted-llm.gguf').write_bytes(b'not a runtime dependency')
        with self.assertRaisesRegex(ValueError, 'Unexpected'):
            stage.check(self.destination, self.root)

    def test_unsafe_inventory_rejected(self):
        name = self.root / 'models/manifest' / stage.GROUPS[0][1]
        content = json.loads(name.read_text())
        content['files'][0]['path'] = '../secret'
        name.write_text(json.dumps(content))
        with self.assertRaisesRegex(ValueError, 'Unsafe'):
            self.run_stage()


if __name__ == '__main__':
    unittest.main()

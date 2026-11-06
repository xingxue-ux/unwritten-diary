#!/usr/bin/env python3
"""把随包本地模型与原生库放进 Windows 便携包（与鸿蒙版同一份模型）。

和 `stage_ohos_models.py` 一个口径：**清单是唯一事实来源**，逐个核对字节数与
SHA-256，核对不过就拒绝出包——不做「先拷了再说」。

布局（相对 `unwritten_diary.exe` 所在目录）：

    onnxruntime.dll                     两条路共用一份 ORT 1.28.2
    onnxruntime_providers_shared.dll
    sherpa-onnx-c-api.dll               转写引擎（core 按名字找）
    sherpa-onnx-cxx-api.dll
    models/asr/sense-voice-int8/{model.int8.onnx,tokens.txt}
    models/semantic/bge-small-zh-v1.5/{model_quantized.onnx,tokenizer.json}

语义那一路的路径要写进 `core-config.json`（运行时由 Dart 侧补），转写那一路
core 自己按约定找。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / 'models/manifest'

# 模型：清单里的每个运行文件。`source` 是仓库里放权重的地方（不入库），
# `target` 是安装包里的相对路径——两个名字不一样是历史原因：语义模型在仓库里叫
# `bge-small-zh-v1.5-ours`（我们自己的导出产物），随包目录按能力命名。
MODEL_GROUPS = (
    ('models/asr/sense-voice-int8', 'models/asr/sense-voice-int8',
     'sense-voice-small-int8-2024-07-17.json'),
    ('models/semantic/bge-small-zh-v1.5', 'models/bge-small-zh-v1.5-ours',
     'bge-small-zh-v1.5.json'),
)

# 原生库：Windows x64 的固定来源与哈希。体积大，不入库，按需下载。
NATIVES = (
    ('onnxruntime.dll', 'onnxruntime-1.28.2-win-x64'),
    ('onnxruntime_providers_shared.dll', 'sherpa-onnx-v1.13.8-win-x64'),
    ('sherpa-onnx-c-api.dll', 'sherpa-onnx-v1.13.8-win-x64'),
    ('sherpa-onnx-cxx-api.dll', 'sherpa-onnx-v1.13.8-win-x64'),
)


def sha256(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def model_inventory() -> list[dict]:
    files = []
    for target_dir, source_dir, name in MODEL_GROUPS:
        manifest = json.loads((MANIFEST / name).read_text(encoding='utf-8-sig'))
        for item in manifest['files']:
            rel = Path(item['path'])
            if rel.is_absolute() or '..' in rel.parts:
                raise ValueError(f'Unsafe manifest path: {rel}')
            files.append({
                'source': f'{source_dir}/{rel.as_posix()}',
                'target': f'{target_dir}/{rel.as_posix()}',
                'bytes': item['bytes'],
                'sha256': item['sha256'],
            })
    return files


def verify(path: Path, item: dict) -> None:
    if not path.is_file():
        raise ValueError(f'Missing: {path}')
    if path.stat().st_size != item['bytes']:
        raise ValueError(f'Wrong size: {path} ({path.stat().st_size} != {item["bytes"]})')
    if sha256(path) != item['sha256']:
        raise ValueError(f'SHA-256 mismatch: {path}')


def stage(models_dir: Path, natives_dir: Path, stage: Path) -> dict:
    inventory = model_inventory()

    def source_of(item: dict) -> Path:
        # `--models-dir` 指向仓库根的 `models/`。
        return models_dir / item['source'].split('/', 1)[1]

    # 先全部校验，再动 stage：半个包比没有包更糟。
    for item in inventory:
        verify(source_of(item), item)
    for name, _ in NATIVES:
        if not (natives_dir / name).is_file():
            raise ValueError(f'Missing native library: {natives_dir / name}')

    for item in inventory:
        source = source_of(item)
        target = stage / item['target']
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        verify(target, item)
    for name, _ in NATIVES:
        shutil.copyfile(natives_dir / name, stage / name)

    # 许可与来源说明随包走（和鸿蒙一致）。
    for source, target in (
        ('sense-voice-LICENSE-MIT.txt', 'models/SenseVoice-LICENSE-MIT.txt'),
        ('bge-small-zh-v1.5-LICENSE-MIT.txt', 'models/BGE-LICENSE-MIT.txt'),
        ('ohos-local-models-NOTICE.md', 'models/NOTICE.md'),
    ):
        shutil.copyfile(MANIFEST / source, stage / target)

    total = sum(item['bytes'] for item in inventory) + sum(
        (natives_dir / name).stat().st_size for name, _ in NATIVES)
    return {'files': len(inventory) + len(NATIVES), 'bytes': total}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--models-dir', type=Path, default=ROOT / 'models')
    parser.add_argument('--natives-dir', type=Path, default=ROOT / 'third_party/windows-native')
    parser.add_argument('--stage', type=Path, required=True)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    try:
        if args.check:
            for item in model_inventory():
                verify(args.stage / item['target'], item)
            for name, _ in NATIVES:
                if not (args.stage / name).is_file():
                    raise ValueError(f'Missing in stage: {name}')
            print('[PASS] 随包模型与原生库齐全')
            return 0
        summary = stage(args.models_dir, args.natives_dir, args.stage)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f'[FAIL] {error}\n')
    print(f'[PASS] {summary["files"]} 个文件，{summary["bytes"]:,} 字节（约 {summary["bytes"]/1024/1024:.1f} MiB）')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

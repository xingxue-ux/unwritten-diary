#!/usr/bin/env python3
"""Stage the pinned ASR, embedding and text generation models for offline OHOS."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]
RAW = ROOT / 'apps/ohos-diary-core/entry/src/main/resources/rawfile/bundled_models'
GROUPS = (
    ('asr/sense-voice-int8', 'sense-voice-small-int8-2024-07-17.json', 'asr'),
    ('semantic/bge-small-zh-v1.5-ours', 'bge-small-zh-v1.5.json', 'bge'),
    ('llm/qwen3.5-4b', 'qwen3.5-4b-q4_k_m.json', 'llm'),
)


def sha256(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def inventory(root: Path = ROOT) -> dict:
    files = []
    for destination, name, _ in GROUPS:
        manifest = json.loads((root / 'models/manifest' / name).read_text(encoding='utf-8-sig'))
        for item in manifest['files']:
            # Only the runtime files explicitly pinned in the canonical manifests.
            path = Path(item['path'])
            if path.is_absolute() or '..' in path.parts:
                raise ValueError(f'Unsafe manifest path: {path}')
            files.append({'path': f'{destination}/{path.as_posix()}',
                          'bytes': item['bytes'], 'sha256': item['sha256']})
    return {'version': 1, 'files': files}


def verify(path: Path, item: dict) -> None:
    if not path.is_file() or path.stat().st_size != item['bytes']:
        raise ValueError(f'Missing/wrong size: {path} (expected {item["bytes"]} bytes)')
    if sha256(path) != item['sha256']:
        raise ValueError(f'SHA-256 mismatch: {path}')


def check(destination: Path = RAW, root: Path = ROOT) -> dict:
    expected = inventory(root)
    manifest = json.loads((destination / 'manifest.json').read_text(encoding='utf-8-sig'))
    if manifest != expected:
        raise ValueError('Bundled model inventory does not match models/manifest')
    for item in expected['files']:
        verify(destination / item['path'], item)
    for name in ('SenseVoice-LICENSE-MIT.txt', 'BGE-LICENSE-MIT.txt', 'Qwen3.5-LICENSE-Apache-2.0.txt', 'llama.cpp-LICENSE-MIT.txt', 'NOTICE.md'):
        if not (destination / name).is_file():
            raise ValueError(f'Missing model notice: {name}')
    allowed = {item['path'] for item in expected['files']} | {
        'manifest.json', 'SenseVoice-LICENSE-MIT.txt', 'BGE-LICENSE-MIT.txt', 'Qwen3.5-LICENSE-Apache-2.0.txt', 'llama.cpp-LICENSE-MIT.txt', 'NOTICE.md'}
    actual = {path.relative_to(destination).as_posix() for path in destination.rglob('*') if path.is_file()}
    if actual - allowed:
        raise ValueError(f'Unexpected bundled files (not pinned runtime models): {sorted(actual - allowed)}')
    return expected


def stage(asr: Path, bge: Path, destination: Path = RAW, root: Path = ROOT, llm: Path | None = None) -> dict:
    manifest = inventory(root)
    # Validate every source before copying anything to the build inputs.
    sources = []
    for group, _, key in GROUPS:
        source = {'asr': asr, 'bge': bge, 'llm': llm or root / 'models/llm/qwen3.5-4b'}[key]
        for item in manifest['files']:
            if item['path'].startswith(group + '/'):
                path = source / Path(item['path']).relative_to(group)
                verify(path, item)
                sources.append((path, item))
    for path, item in sources:
        target = destination / item['path']
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.is_file() or target.stat().st_size != item['bytes'] or sha256(target) != item['sha256']:
            part = target.with_name(target.name + '.part')
            shutil.copyfile(path, part)
            verify(part, item)
            part.replace(target)
    for source, name in (
        ('sense-voice-LICENSE-MIT.txt', 'SenseVoice-LICENSE-MIT.txt'),
        ('bge-small-zh-v1.5-LICENSE-MIT.txt', 'BGE-LICENSE-MIT.txt'),
        ('ohos-local-models-NOTICE.md', 'NOTICE.md'),
        ('qwen3.5-LICENSE-Apache-2.0.txt', 'Qwen3.5-LICENSE-Apache-2.0.txt'),
        ('llama.cpp-LICENSE-MIT.txt', 'llama.cpp-LICENSE-MIT.txt'),
    ):
        shutil.copyfile(root / 'models/manifest' / source, destination / name)
    part = destination / 'manifest.json.part'
    part.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    part.replace(destination / 'manifest.json')
    return check(destination, root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--asr-dir', type=Path, default=Path(os.environ.get('DIARY_ASR_MODEL_DIR', ROOT / 'models/asr/sense-voice-int8')))
    parser.add_argument('--bge-dir', type=Path, default=ROOT / 'models/bge-small-zh-v1.5-ours')
    parser.add_argument('--llm-dir', type=Path, default=ROOT / 'models/llm/qwen3.5-4b')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    try:
        manifest = check() if args.check else stage(args.asr_dir, args.bge_dir, llm=args.llm_dir)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f'[FAIL] {error}\nStage the pinned models with --asr-dir and --bge-dir before building HAP.\n')
    print(f'[PASS] {len(manifest["files"])} pinned local model files, {sum(f["bytes"] for f in manifest["files"]):,} bytes; in-process offline LLM included.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

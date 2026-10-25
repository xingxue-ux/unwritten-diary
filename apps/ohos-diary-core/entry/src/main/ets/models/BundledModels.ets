import { common } from '@kit.AbilityKit';
import { fileIo as fs, hash } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { DiaryCore } from '../core/DiaryCore';
import { ProviderConfig, ProviderIdParams, SaveProviderParams, TestProviderParams } from '../core/Models';
import { CoreGateway } from '../ui/CoreGateway';

interface ModelFile {
  path: string;
  bytes: number;
  sha256: string;
}

interface ModelManifest {
  version: number;
  files: ModelFile[];
}

export interface ModelProgress {
  completed: number;
  total: number;
}

const RESOURCE_ROOT: string = 'bundled_models';
const ASR_REL: string = 'asr/sense-voice-int8';
const BGE_REL: string = 'semantic/bge-small-zh-v1.5-ours';
const BUILTIN_ASR: string = 'builtin_local_asr';
const CHUNK_BYTES: number = 1024 * 1024;

/** Offline model deployment; completes before any product page opens the core. */
export class BundledModels {
  static async prepare(context: common.UIAbilityContext,
    progress: (value: ModelProgress) => void, filesDir: string = context.filesDir): Promise<void> {
    if (filesDir !== context.filesDir && !filesDir.startsWith(`${context.filesDir}/`)) {
      throw new Error('模型部署目录必须在应用沙箱内');
    }
    const content = await context.resourceManager.getRawFileContent(`${RESOURCE_ROOT}/manifest.json`);
    const text: string = new util.TextDecoder('utf-8').decodeWithStream(content);
    const manifest: ModelManifest = JSON.parse(text) as ModelManifest;
    if (manifest.version !== 1 || manifest.files.length !== 4) {
      throw new Error('随包模型清单不完整');
    }
    let total: number = 0;
    for (const file of manifest.files) {
      if (!/^(asr|semantic)\/[a-zA-Z0-9._-]+\/[a-zA-Z0-9._-]+$/.test(file.path) ||
        file.path.includes('..') || file.bytes <= 0 || !/^[a-f0-9]{64}$/.test(file.sha256)) {
        throw new Error('随包模型清单无效');
      }
      total += file.bytes;
    }
    const root: string = `${filesDir}/bundled-models/v1`;
    let completed: number = 0;
    progress({ completed: completed, total: total });
    for (const file of manifest.files) {
      const base: number = completed;
      await BundledModels.deploy(context, root, file, (copied: number) => {
        progress({ completed: base + copied, total: total });
      });
      completed += file.bytes;
      progress({ completed: completed, total: total });
    }
    // Only missing semantic defaults are filled; custom paths/unknown fields survive.
    // ASR is a provider override so a user's later model selection still takes effect.
    await BundledModels.semanticDefaults(filesDir, root);
    const marker: string = `${root}/defaults-ready.json`;
    if (!BundledModels.exists(marker)) {
      await BundledModels.asrDefault(filesDir, `${root}/${ASR_REL}`);
      await BundledModels.writeAtomic(marker, '{"version":1}\n');
    }
    const selectionMarker: string = `${root}/online-generation-selection-v1.json`;
    if (!BundledModels.exists(selectionMarker)) {
      await BundledModels.normalizeOnlineSelection(filesDir);
      await BundledModels.writeAtomic(selectionMarker, '{"version":1}\n');
    }
    hilog.info(0, 'DiaryModels', 'BUNDLED_MODELS_READY files=%{public}d bytes=%{public}d',
      manifest.files.length, total);
  }

  // Repair the old independent switches once. Preserve test-status and creation
  // priority; never activate a disabled service or send a network request.
  private static async normalizeOnlineSelection(filesDir: string): Promise<void> {
    const db: string = `${filesDir}/library.sqlite`;
    const listed = await CoreGateway.call(db, 'providers.list', null);
    if (!listed.ok) { throw new Error('读取整理服务失败，请重试'); }
    const enabled: ProviderConfig[] = (listed.data as ProviderConfig[]).filter((p: ProviderConfig) =>
      p.adapter_id === 'openai_compatible' && p.enabled && p.capabilities.includes('generate_text'));
    if (enabled.length <= 1) { return; }
    const statuses: string[] = ['ok', 'untested', 'failed'];
    for (const status of statuses) {
      const preferred: ProviderConfig | undefined = enabled.find((p: ProviderConfig) => p.test_status === status);
      if (preferred !== undefined) {
        const params: ProviderIdParams = { provider_id: preferred.id };
        const selected = await CoreGateway.call(db, 'providers.selectGeneration', params);
        if (!selected.ok) { throw new Error('切换整理服务失败，请重试'); }
        return;
      }
    }
  }

  private static exists(path: string): boolean {
    try { return fs.accessSync(path); } catch (_) { return false; }
  }

  private static async matches(path: string, file: ModelFile): Promise<boolean> {
    if (!BundledModels.exists(path) || fs.statSync(path).size !== file.bytes) { return false; }
    return (await hash.hash(path, 'sha256')).toLowerCase() === file.sha256;
  }

  private static async deploy(context: common.UIAbilityContext, root: string,
    file: ModelFile, progress: (bytes: number) => void): Promise<void> {
    const target: string = `${root}/${file.path}`;
    if (await BundledModels.matches(target, file)) { return; }
    const directory: string = target.substring(0, target.lastIndexOf('/'));
    if (!BundledModels.exists(directory)) { fs.mkdirSync(directory, true); }
    const part: string = `${target}.part`;
    const resource: string = `${RESOURCE_ROOT}/${file.path}`;
    const raw = await context.resourceManager.getRawFd(resource);
    let output: fs.File | null = null;
    try {
      if (raw.length !== file.bytes) { throw new Error('随包模型大小与清单不一致'); }
      output = await fs.open(part, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
      // raw.fd points into the HAP, not necessarily at byte zero. Use sequential
      // reads after one absolute seek, avoiding offset accumulation between chunks.
      fs.lseek(raw.fd, raw.offset, fs.WhenceType.SEEK_SET);
      const buffer: ArrayBuffer = new ArrayBuffer(CHUNK_BYTES);
      let copied: number = 0;
      while (copied < file.bytes) {
        const count: number = await fs.read(raw.fd, buffer, { length: Math.min(CHUNK_BYTES, file.bytes - copied) });
        if (count <= 0) { throw new Error('随包模型读取提前结束'); }
        await BundledModels.writeBytes(output.fd, buffer.slice(0, count));
        copied += count;
        progress(copied);
      }
      await fs.fsync(output.fd);
      await fs.close(output);
      output = null;
      if (!await BundledModels.matches(part, file)) { throw new Error('随包模型校验失败'); }
      await fs.rename(part, target);
    } finally {
      if (output !== null) { await fs.close(output); }
      await context.resourceManager.closeRawFd(resource);
      if (BundledModels.exists(part)) { fs.unlinkSync(part); }
    }
  }

  private static async writeBytes(fd: number, bytes: ArrayBuffer): Promise<void> {
    let written: number = 0;
    while (written < bytes.byteLength) {
      const count: number = await fs.write(fd, bytes.slice(written));
      if (count <= 0) { throw new Error('写入本地模型失败，请检查可用空间'); }
      written += count;
    }
  }

  private static async writeAtomic(path: string, text: string): Promise<void> {
    const part: string = `${path}.part`;
    const output: fs.File = await fs.open(part, fs.OpenMode.CREATE | fs.OpenMode.READ_WRITE | fs.OpenMode.TRUNC);
    try {
      const bytes: Uint8Array = new util.TextEncoder().encodeInto(text);
      await BundledModels.writeBytes(output.fd, bytes.buffer as ArrayBuffer);
      await fs.fsync(output.fd);
    } catch (error) {
      if (BundledModels.exists(part)) { fs.unlinkSync(part); }
      throw new Error((error as Error).message);
    } finally { await fs.close(output); }
    await fs.rename(part, path);
  }

  private static async semanticDefaults(filesDir: string, root: string): Promise<void> {
    const path: string = `${filesDir}/core-config.json`;
    const existed: boolean = BundledModels.exists(path);
    const previous: string = existed ? await fs.readText(path) : '{}';
    const config: Record<string, Object | null> = JSON.parse(previous) as Record<string, Object | null>;
    if (config === null || Array.isArray(config) || typeof config !== 'object') {
      throw new Error('本机模型配置格式不正确，请先修复配置');
    }
    let changed: boolean = false;
    if (config['modelDir'] === undefined && config['model_dir'] === undefined) {
      config['modelDir'] = `${root}/${BGE_REL}`;
      changed = true;
    }
    if (config['ortDylib'] === undefined && config['ort_dylib'] === undefined) {
      config['ortDylib'] = `${DiaryCore.nativeLibraryDir()}/libonnxruntime-diary-1.29.1.so`;
      changed = true;
    }
    if (changed) {
      // Do not silently overwrite a concurrent configuration edit.
      if (BundledModels.exists(path) !== existed || (existed && await fs.readText(path) !== previous)) {
        throw new Error('模型配置已变更，请重试');
      }
      await BundledModels.writeAtomic(path, JSON.stringify(config, null, 2) + '\n');
    }
  }

  private static async asrDefault(filesDir: string, directory: string): Promise<void> {
    const db: string = `${filesDir}/library.sqlite`;
    const listed = await CoreGateway.call(db, 'providers.list', null);
    if (!listed.ok) { throw new Error('读取本机转写配置失败'); }
    const providers: ProviderConfig[] = listed.data as ProviderConfig[];
    for (const provider of providers) {
      if (provider.adapter_id === 'sherpa_onnx' &&
        (provider.id !== BUILTIN_ASR || !provider.enabled ||
          (provider.model !== null && provider.model !== directory))) {
        return; // Keep existing choices, including a disabled local engine.
      }
    }
    const config: ProviderConfig = {
      id: BUILTIN_ASR, label: '本机转写（随包模型）', adapter_id: 'sherpa_onnx',
      endpoint: null, model: directory, capabilities: ['transcribe_audio'],
      credential_ref: null, enabled: true, test_status: 'untested',
    };
    const params: SaveProviderParams = { config: config, secret: null };
    const saved = await CoreGateway.call(db, 'providers.save', params);
    if (!saved.ok) { throw new Error('设置随包转写模型失败'); }
    // This is the local sherpa adapter: real silent-audio inference, no network.
    const request: TestProviderParams = { provider_id: BUILTIN_ASR, scope: ['transcribe_audio'] };
    const tested = await CoreGateway.call(db, 'providers.test', request);
    if (!tested.ok) { throw new Error('随包转写模型未能加载，请重试'); }
    const refreshed = await CoreGateway.call(db, 'providers.list', null);
    if (!refreshed.ok) { throw new Error('无法确认本机转写模型状态'); }
    for (const provider of refreshed.data as ProviderConfig[]) {
      if (provider.id === BUILTIN_ASR && provider.test_status === 'ok') { return; }
    }
    throw new Error('随包转写模型测试未通过');
  }
}

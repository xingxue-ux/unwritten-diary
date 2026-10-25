const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const ts = require(process.env.OHOS_TYPESCRIPT_PATH || 'typescript');
const digest = data => crypto.createHash('sha256').update(data).digest('hex');
const base = '/app/bundled-models/v1';
const asr = `${base}/asr/sense-voice-int8`;
const marker = `${base}/defaults-ready.json`;

function environment() {
  const files = new Map(), handles = new Map(), calls = [], resources = new Map(), directories = new Set();
  const state = {opens: 0, closes: 0, failWrite: false, corruptResource: false, testFails: false};
  let nextFd = 1;
  const names = ['asr/sense-voice-int8/model.int8.onnx', 'asr/sense-voice-int8/tokens.txt',
    'semantic/bge-small-zh-v1.5-ours/model_quantized.onnx', 'semantic/bge-small-zh-v1.5-ours/tokenizer.json'];
  const inventory = {version: 1, files: names.map((name, index) => {
    const bytes = Buffer.alloc(index === 0 ? 2 * 1024 * 1024 + 31 : 100 + index, index + 1);
    resources.set('bundled_models/' + name, bytes);
    return {path: name, bytes: bytes.length, sha256: digest(bytes)};
  })};
  const originalProvider = {id: 'builtin_local_asr', adapter_id: 'sherpa_onnx', model: null,
    enabled: true, test_status: 'untested'};
  let providers = [originalProvider];
  const io = {
    OpenMode: {CREATE: 1, READ_WRITE: 2, TRUNC: 4}, WhenceType: {SEEK_SET: 0},
    accessSync: name => files.has(name) || directories.has(name), statSync: name => ({size: files.get(name).length}),
    mkdirSync: name => {if (directories.has(name)) throw new Error('File exists'); directories.add(name);},
    unlinkSync: name => files.delete(name),
    open: async name => {files.set(name, Buffer.alloc(0)); const fd = nextFd++; handles.set(fd, {name, position: 0}); return {fd};},
    lseek: (fd, offset) => {handles.get(fd).position = offset;},
    read: async (fd, buffer, options) => {
      const handle = handles.get(fd), data = handle.data;
      const count = Math.min(options.length, data.length - handle.position, 600001);
      new Uint8Array(buffer).set(data.subarray(handle.position, handle.position + count));
      handle.position += count; return count;
    },
    write: async (fd, buffer) => {
      if (state.failWrite) throw new Error('disk full');
      const handle = handles.get(fd), bytes = Buffer.from(buffer);
      const count = Math.min(bytes.length, 100003); // exercise partial writes
      files.set(handle.name, Buffer.concat([files.get(handle.name), bytes.subarray(0, count)])); return count;
    },
    fsync: async () => {}, close: async file => {handles.delete(file.fd);},
    rename: async (a, b) => {files.set(b, files.get(a)); files.delete(a);},
    readText: async name => files.get(name).toString('utf8'),
  };
  const gateway = {call: async (db, method, params) => {
    calls.push({method, params: JSON.parse(JSON.stringify(params))});
    assert.equal(db, '/app/library.sqlite');
    if (method === 'providers.list') return {ok: true, data: providers.map(p => ({...p}))};
    if (method === 'providers.save') {
      assert.equal(params.config.adapter_id, 'sherpa_onnx');
      assert.equal(params.secret, null);
      providers = [params.config]; return {ok: true, data: params.config};
    }
    if (method === 'providers.test') {
      assert.deepEqual(Array.from(params.scope), ['transcribe_audio']);
      if (state.testFails) return {ok: false};
      providers[0].test_status = 'ok'; return {ok: true};
    }
    throw new Error('Unexpected API ' + method);
  }};
  const manager = {
    getRawFileContent: async () => Buffer.from(JSON.stringify(inventory)),
    getRawFd: async name => {
      state.opens++;
      const bytes = Buffer.from(resources.get(name));
      if (state.corruptResource) bytes[0] ^= 255;
      const header = Buffer.alloc(173, 99), fd = nextFd++;
      handles.set(fd, {data: Buffer.concat([header, bytes]), position: 0, raw: true});
      return {fd, offset: header.length, length: bytes.length};
    },
    closeRawFd: async () => {
      state.closes++;
      for (const [fd, handle] of handles) if (handle.raw) handles.delete(fd);
    },
  };
  const modules = {
    '@kit.CoreFileKit': {fileIo: io, hash: {hash: async name => digest(files.get(name))}},
    '@kit.ArkTS': {util: {
      TextDecoder: class {decodeWithStream(bytes) {return Buffer.from(bytes).toString('utf8');}},
      TextEncoder: class {encodeInto(text) {return new TextEncoder().encode(text);}},
    }},
    '@kit.PerformanceAnalysisKit': {hilog: {info: () => {}}},
    '../core/DiaryCore': {DiaryCore: {nativeLibraryDir: () => '/native'}},
    '../ui/CoreGateway': {CoreGateway: gateway},
  };
  const source = fs.readFileSync(path.resolve(__dirname, '../apps/ohos-diary-core/entry/src/main/ets/models/BundledModels.ets'), 'utf8');
  const js = ts.transpileModule(source, {compilerOptions: {target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.CommonJS}}).outputText;
  const context = {exports: {}, require: name => modules[name] || {}, ArrayBuffer, Uint8Array, Error, JSON, Math};
  vm.runInNewContext(js, context);
  const prepare = () => context.exports.BundledModels.prepare({filesDir: '/app', resourceManager: manager}, () => {});
  return {prepare, files, calls, state, inventory, providers: value => {providers = value;}, handles};
}

async function firstInstall() {
  const env = environment(); await env.prepare();
  assert.equal(env.state.opens, 4); assert.equal(env.state.closes, 4); assert.equal(env.handles.size, 0);
  for (const file of env.inventory.files) assert.equal(digest(env.files.get(`${base}/${file.path}`)), file.sha256);
  const config = JSON.parse(env.files.get('/app/core-config.json'));
  assert.equal(config.ortDylib, '/native/libonnxruntime-diary-1.29.1.so');
  assert.equal(config.modelDir, `${base}/semantic/bge-small-zh-v1.5-ours`);
  assert.equal(config.asrModelDir, undefined); // explicit provider selection keeps priority
  assert.ok(env.files.has(marker));
  assert.equal(env.calls.find(c => c.method === 'providers.save').params.config.model, asr);
}
async function restartPreservesChoice() {
  const env = environment(); await env.prepare();
  const calls = env.calls.length;
  env.providers([{id: 'builtin_local_asr', adapter_id: 'sherpa_onnx', model: asr, enabled: false}]);
  await env.prepare(); assert.equal(env.calls.length, calls); assert.equal(env.state.opens, 4);
}
async function customConfiguration() {
  const env = environment(), text = '{"model_dir":"/custom/embed","ort_dylib":"/custom/ort","asrModelDir":"/custom/asr","extra":42}';
  env.files.set('/app/core-config.json', Buffer.from(text));
  env.providers([{id: 'mine', adapter_id: 'sherpa_onnx', model: '/custom/asr', enabled: false}]);
  await env.prepare(); assert.equal(env.files.get('/app/core-config.json').toString(), text);
  assert.deepEqual(env.calls.map(c => c.method), ['providers.list']);
}
async function interruptedCopy() {
  const env = environment(), destination = `${base}/${env.inventory.files[0].path}`;
  env.files.set(destination, Buffer.from('previous')); env.state.failWrite = true;
  await assert.rejects(env.prepare(), /disk full/);
  assert.equal(env.files.get(destination).toString(), 'previous');
  assert.equal(env.files.has(destination + '.part'), false); assert.equal(env.handles.size, 0);
  assert.equal(env.files.has(marker), false); env.state.failWrite = false; await env.prepare();
  assert.equal(digest(env.files.get(destination)), env.inventory.files[0].sha256);
}
async function corruptResource() {
  const env = environment(); env.state.corruptResource = true;
  await assert.rejects(env.prepare(), /校验失败/);
  assert.equal(env.files.has(marker), false); assert.equal(env.handles.size, 0);
  assert.equal([...env.files.keys()].some(p => p.endsWith('.part')), false);
}
async function invalidConfiguration() {
  const env = environment(); env.files.set('/app/core-config.json', Buffer.from('{invalid'));
  await assert.rejects(env.prepare()); assert.equal(env.files.get('/app/core-config.json').toString(), '{invalid');
  assert.equal(env.calls.length, 0); assert.equal(env.files.has(marker), false);
}
async function failedLocalTestRetries() {
  const env = environment(); env.state.testFails = true;
  await assert.rejects(env.prepare(), /未能加载/); assert.equal(env.files.has(marker), false);
  env.state.testFails = false; await env.prepare(); assert.equal(env.files.has(marker), true);
}
async function unsafeManifest() {
  const env = environment(); env.inventory.files[0].path = '../library.sqlite';
  await assert.rejects(env.prepare(), /清单无效/); assert.equal(env.files.size, 0);
}
(async () => {
  for (const test of [firstInstall, restartPreservesChoice, customConfiguration, interruptedCopy,
    corruptResource, invalidConfiguration, failedLocalTestRetries, unsafeManifest]) {
    await test(); console.log('PASS ' + test.name);
  }
})().catch(error => {console.error(error); process.exitCode = 1;});

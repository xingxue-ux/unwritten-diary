const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),assert=require('node:assert/strict');
const ts=require(process.env.OHOS_TYPESCRIPT_PATH||'typescript');
const root=path.resolve(__dirname,'../apps/ohos-diary-core/entry/src/main/ets');
function load(file,modules,globals={}) {
  let source=fs.readFileSync(path.join(root,file),'utf8');
  if(file.startsWith('pages/')) source=source.slice(0,source.indexOf('  build()'))+'}\n';
  source=source.replace('@Entry','').replace('@Component','').replace('struct Startup','export class Startup').replace(/@State\s*/g,'');
  const code=ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.CommonJS}}).outputText;
  const ctx={exports:{},require:n=>modules[n]||{},...globals};vm.runInNewContext(code,ctx);return ctx.exports;
}
const service=load('ui/OnlineService.ets',{});
assert.equal(service.isOfficialEndpoint('https://api.deepseek.com/v1/'),true);
for(const s of [null,'http://api.deepseek.com','https://api.deepseek.com.evil.test','https://api.deepseek.com@evil.test','https://api.deepseek.com?x=1'])assert.equal(service.isOfficialEndpoint(s),false);
async function startup(answer,alreadyAccepted=false,writeFails=false) {
  let prompts=0,prepared=0,terminated=0,written=false;
  const io={accessSync:p=>p==='/app'||alreadyAccepted,OpenMode:{CREATE:1,READ_WRITE:2,TRUNC:4},
    openSync:()=>({fd:1}),writeSync:()=>{if(writeFails)throw Error('full');written=true;},closeSync:()=>{}};
  const {Startup}=load('pages/Startup.ets',{'@kit.CoreFileKit':{fileIo:io},
    '@kit.ArkUI':{promptAction:{showDialog:async()=>{prompts++;return {index:answer};}}},
    '../session/AppDirectory':{appFilesDir:()=>'/app'},'../ui/OnlineService':service},
    {getContext:()=>({terminateSelf:async()=>{terminated++;}})});
  const page=new Startup();page.prepare=async()=>{prepared++;};await page.begin();
  return {prompts,prepared,terminated,written,noticeFailed:page.noticeFailed};
}
(async()=>{
  assert.deepEqual(await startup(0),{prompts:1,prepared:0,terminated:1,written:false,noticeFailed:false});
  assert.deepEqual(await startup(1),{prompts:1,prepared:1,terminated:0,written:true,noticeFailed:false});
  assert.deepEqual(await startup(1,true),{prompts:0,prepared:1,terminated:0,written:false,noticeFailed:false});
  assert.equal((await startup(1,false,true)).prepared,0);
  const calls=[];
  class DiaryError extends Error {constructor(code,message){super(message);this.code=code;this.retryable=false;}static fromThrown(e){return e;}}
  const worker=load('ui/CoreWorker.ets',{'@kit.PerformanceAnalysisKit':{hilog:{debug:()=>{}}},
    '@kit.ArkTS':{process:{tid:1}},'../core/DiaryError':{DiaryError},
    '../core/DiaryCore':{DiaryCore:{openHandleCount:()=>1}},
    './CoreSessions':{acquireCore:()=>({rawCall:(method)=>{calls.push(method);return null;}})}});
  const results=JSON.parse(worker.runCoreBatch('/app/library.sqlite',JSON.stringify([
    {method:'settings.get',params:''},{method:'exports.create',params:'{}'},{method:'providers.list',params:''}])));
  assert.deepEqual(calls,['settings.get','providers.list']);
  assert.equal(results[1].error.code,'unsupported_format');assert.equal(results[2].ok,true);
  console.log('PASS official endpoints and first-launch notice: decline, accept, repeat, write failure');
  console.log('PASS export blocked before native call; other requests continue');
})().catch(e=>{console.error(e);process.exitCode=1;});

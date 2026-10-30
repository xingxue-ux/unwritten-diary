const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),assert=require('node:assert/strict');
const ts=require(process.env.OHOS_TYPESCRIPT_PATH||'typescript');
const root=path.resolve(__dirname,'../apps/ohos-diary-core/entry/src/main/ets');
function load(file,modules,globals={}) {
  let source=fs.readFileSync(path.join(root,file),'utf8');
  if(file.startsWith('pages/')) {
    const builder=source.indexOf('  @Builder');
    source=source.slice(0,builder>=0?builder:source.indexOf('  build()'))+'}\n';
  }
  source=source.replace('@Entry','').replace('@Component','').replace('struct Startup','export class Startup')
    .replace('export struct SnippetsPage','export class SnippetsPage')
    .replace(/@(State|Prop|StorageLink\([^)]*\)|Watch\([^)]*\))\s*/g,'');
  const code=ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.CommonJS}}).outputText;
  const ctx={exports:{},require:n=>modules[n]||{},...globals};vm.runInNewContext(code,ctx);return ctx.exports;
}
const service=load('ui/LocalService.ets',{});
assert.ok(service.SERVICE_NOTICE.includes('不提供自动生成或改写日记'));
assert.ok(service.LOCAL_PRIVACY.includes('语音转写和语义搜索使用随包本地模型'));
assert.ok(!fs.readFileSync(path.join(root,'../module.json5'),'utf8').includes('ohos.permission.INTERNET'));
for(const file of ['pages/Index.ets','pages/ProfilePage.ets']) {
  const source=fs.readFileSync(path.join(root,file),'utf8');
  for(const removed of ['LlmConfigPage','StylesPage','SchedulePage',"'llm'","'styles'","'schedule'"])assert.ok(!source.includes(removed),file+': '+removed);
}
async function startup(answer,alreadyAccepted=false,writeFails=false) {
  let prompts=0,prepared=0,terminated=0,written=false;
  const io={accessSync:p=>p==='/app'||alreadyAccepted,OpenMode:{CREATE:1,READ_WRITE:2,TRUNC:4},
    openSync:()=>({fd:1}),writeSync:()=>{if(writeFails)throw Error('full');written=true;},closeSync:()=>{}};
  const {Startup}=load('pages/Startup.ets',{'@kit.CoreFileKit':{fileIo:io},
    '@kit.ArkUI':{promptAction:{showDialog:async()=>{prompts++;return {index:answer};}}},
    '../session/AppDirectory':{appFilesDir:()=>'/app'},'../ui/LocalService':service},
    {getContext:()=>({terminateSelf:async()=>{terminated++;}})});
  const page=new Startup();page.prepare=async()=>{prepared++;};await page.begin();
  return {prompts,prepared,terminated,written,noticeFailed:page.noticeFailed};
}
(async()=>{
  const {SnippetsPage}=load('pages/SnippetsPage.ets',{
    '../ui/Errors':{EMPTY_UI_ERROR:{}},'../ui/AppRuntime':{databasePath:()=>'/app/library.sqlite'}
  },{getContext:()=>({}),NavPathStack:class{}});
  for(const offset of [-400,-1,0,1]) {
    const date=new Date();date.setDate(date.getDate()+offset);
    const selected=`${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;
    const page=new SnippetsPage();page.selectedDay=selected;page.reload=()=>{};page.aboutToAppear();
    assert.equal(page.dayOffset,offset,'日记入口必须打开选中的日期');
  }
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
    {method:'settings.get',params:''},{method:'exports.create',params:'{}'},{method:'providers.list',params:''},
    {method:'diary.generate',params:'{}'},{method:'providers.models',params:'{}'},
    {method:'providers.selectGeneration',params:'{}'},{method:'organize.settings',params:''},
    {method:'styles.list',params:''},{method:'sources.extract',params:'{}'},
    {method:'indexes.rebuildSemantic',params:'{}'}])));
  assert.deepEqual(calls,['settings.get','providers.list','sources.extract','indexes.rebuildSemantic']);
  assert.equal(results[1].error.code,'unsupported_format');assert.equal(results[2].ok,true);
  for(const i of [3,4,5,6,7])assert.equal(results[i].error.code,'invalid_argument');
  assert.equal(results[8].ok,true);assert.equal(results[9].ok,true);
  console.log('PASS local privacy notice: decline, accept, repeat, write failure');
  console.log('PASS generation blocked; local extraction and semantic indexing continue');
  console.log('PASS day records open the selected historical/current/future date');
})().catch(e=>{console.error(e);process.exitCode=1;});

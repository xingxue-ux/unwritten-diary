const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const ts = require(process.env.OHOS_TYPESCRIPT_PATH || 'typescript');
const ets = path.resolve(__dirname, '../apps/ohos-diary-core/entry/src/main/ets');
const empty = {code:'',message:'',title:'',hint:'',retryable:false,native:false};
const noop = () => {};
const log = {info:noop,warn:noop,error:noop,debug:noop};
function deferred(){let resolve;const promise=new Promise(r=>{resolve=r;});return {promise,resolve};}
class DiaryError extends Error {
  constructor(code,message){super(message);this.code=code;}
  static fromThrown(e){return e instanceof DiaryError?e:new DiaryError('native_unavailable',e.message);}
  toLogLine(){return 'code='+this.code+' message='+this.message;}
}
const errorModule={EMPTY_UI_ERROR:empty,uiErrorFromThrown:e=>({...empty,code:e.code||'error',message:e.message})};
// These tests run real controller methods. Platform APIs are mocks; ArkUI builders
// are excluded, so passing does not replace HAP compilation or device verification.
function load(file,modules,globals={},page=false){
  let source=fs.readFileSync(path.join(ets,file),'utf8');
  if(page){
    const cut=source.indexOf('  @Builder');
    source=source.slice(0,cut>=0?cut:source.indexOf('  build()'))+'\n}\n';
    source=source.replace('@Component','').replace('export struct ','export class ')
      .replace(/@(State|StorageProp\([^)]*\)|StorageLink\([^)]*\)|Watch\([^)]*\))\s*/g,'');
  }
  const js=ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.CommonJS}}).outputText;
  const ctx={exports:{},require:name=>modules[name]||{},console,Date,Math,Error,Map,ArrayBuffer,Uint8Array,
    NavPathStack:class{},setInterval,clearInterval,...globals};
  vm.runInNewContext(js,ctx,{filename:file});return ctx.exports;
}
function recorderEnv(stage=''){
  const gate=deferred(),entered=deferred();
  const state={starts:0,running:false,hold:false,background:false,callbacks:{},writeFails:false};
  async function pause(point){if(stage===point){entered.resolve();await gate.promise;}}
  class Continuous{
    async start(){await pause('background');state.background=true;return {ok:true};}
    async stop(){state.background=false;}
  }
  class Screen{
    async enable(){await pause('screen');state.hold=true;return {ok:true};}
    async disable(){state.hold=false;}
  }
  class Writer{
    open(){}byteSize(){return 44;}pcmBytes(){return 0;}abort(){}
    writePcm(){if(state.writeFails)throw new Error('disk full');}
  }
  const cap={on:(name,cb)=>{state.callbacks[name]=cb;},
    start:async()=>{await pause('start');state.starts++;state.running=true;},
    stop:async()=>{state.running=false;},release:async()=>{state.running=false;}};
  const audio={AudioSamplingRate:{SAMPLE_RATE_16000:16000},AudioChannel:{CHANNEL_1:1},
    AudioSampleFormat:{SAMPLE_FORMAT_S16LE:1},AudioEncodingType:{ENCODING_TYPE_RAW:0},
    SourceType:{SOURCE_TYPE_MIC:0},AudioState:{STATE_RUNNING:2},
    createAudioCapturer:async()=>{await pause('create');return cap;}};
  const modules={
    '@kit.AudioKit':{audio},'@kit.CoreFileKit':{fileIo:{}},
    '@kit.BasicServicesKit':{systemDateTime:{getUptime:()=>1000000000,
      getTimezoneSync:()=> 'Asia/Shanghai',TimeType:{STARTUP:0}}},
    '@kit.PerformanceAnalysisKit':{hilog:log},'../core/DiaryError':{DiaryError},
    './ContinuousTask':{ContinuousTaskController:Continuous,ContinuousTaskResult:class{}},
    './ScreenKeepOn':{ScreenKeepOn:Screen,KeepScreenOnResult:class{}},
    './PcmSegmentWriter':{PcmSegmentWriter:Writer},
    './RecordingFormat':{SAMPLE_RATE:16000,CHANNEL_COUNT:1,BITS_PER_SAMPLE:16,
      msToPcmBytes:x=>x*32,pcmBytesToMs:x=>x/32}};
  const {VoiceRecorder,RecorderOptions}=load('recording/VoiceRecorder.ets',modules);
  const core={createDraft:()=>({id:'draft'}),prepareRecording:()=>({
    recording_id:'rec',asset_id:'asset',staging_ticket:'/sandbox/staging'}),updateRecordingState:noop};
  return {state,gate,entered,recorder:new VoiceRecorder(core,{},new RecorderOptions())};
}
async function cancelledStartup(){
  for(const stage of ['background','screen','create','start']){
    const env=recorderEnv(stage),pending=env.recorder.start();
    const rejected=assert.rejects(pending,/录音已取消/);
    await env.entered.promise;await env.recorder.abort();env.gate.resolve();await rejected;
    assert.equal(env.recorder.currentPhase(),'aborted',stage);
    assert.equal(env.state.running,false,stage);
    assert.equal(env.state.hold,false,stage);assert.equal(env.state.background,false,stage);
    if(stage!=='start')assert.equal(env.state.starts,0,stage);
  }
}
async function recordingErrors(){
  for(const kind of ['write','interrupt']){
    const {state,recorder}=recorderEnv();await recorder.start();let tick;
    const {RecordingPage}=load('pages/RecordingPage.ets',{'../ui/Errors':errorModule},
      {setInterval:cb=>{tick=cb;return 1;},clearInterval:noop},true);
    const page=new RecordingPage();page.phase='recording';page.recorder=recorder;page.startTimer();
    if(kind==='write'){state.writeFails=true;state.callbacks.readData(new ArrayBuffer(320));}
    else state.callbacks.stateChange(3);
    tick();assert.equal(page.phase,'error',kind);assert.notEqual(page.error.code,'',kind);
    assert.equal(page.timer,-1,kind);assert.equal(page.interrupted,kind==='interrupt');
    await recorder.abort();
  }
}
async function leaveDuringPermission(){
  const permission=deferred();let opened=0;
  const {RecordingPage}=load('pages/RecordingPage.ets',{
    '../ui/Errors':errorModule,
    '../recording/MicPermission':{ensureMicrophonePermission:()=>permission.promise},
    '../ui/CoreSession':{CoreSession:{open:()=>{opened++;}}}}, {},true);
  const page=new RecordingPage();page.active=true;page.context={};
  const pending=page.startRecording();assert.equal(page.phase,'preparing');
  page.aboutToDisappear();permission.resolve({granted:true});await pending;
  assert.equal(opened,0);assert.equal(page.recorder,null);
}
async function editorState(){
  for(const dirty of [false,true]){
    const {RecordPage}=load('pages/RecordPage.ets',{
      '../ui/Errors':errorModule,'../ui/CoreGateway':{CoreGateway:{call:async()=>({
        ok:true,data:{id:'capture',revision:3,state:'committed',draft_text:'saved text'}})}}},{},true);
    const page=new RecordPage();page.captureId='capture';page.revision=2;page.savedState='draft';
    page.savedText='saved text';page.text=dirty?'unsaved edit':'saved text';
    await page.refreshCapture();assert.equal(page.revision,3);assert.equal(page.savedState,'committed');
    assert.equal(page.canSave(),false);
    if(dirty){assert.equal(page.preservedText,'unsaved edit');page.savePreservedAsNew();
      assert.equal(page.captureId,'');assert.equal(page.text,'unsaved edit');}
  }
}
function snapshot(phase,results,cursor){
  return {session_id:'s',phase,results,cursor,index_coverage:'complete',warnings:[],query_revision:1};
}
async function searchTransitions(){
  const next=deferred(),up=deferred(),cancel=deferred(),calls=[];
  const {SearchPage}=load('pages/SearchPage.ets',{'../ui/Errors':errorModule,
    '../ui/CoreGateway':{CoreGateway:{call:(db,method)=>{calls.push(method);
      return method==='search.nextPage'?next.promise:method==='search.upgrade'?up.promise:cancel.promise;}}}},{},true);
  const page=new SearchPage();page.sessionId='s';page.hasMore=true;page.cursor='k-cursor';page.results=['k1'];
  const paging=page.loadMore();await page.upgrade();assert.deepEqual(calls,['search.nextPage']);
  next.resolve({ok:true,data:snapshot('keyword_ready',['k2'],'k-next')});await paging;
  const upgrading=page.upgrade();await page.loadMore();
  assert.deepEqual(calls,['search.nextPage','search.upgrade']);
  const cancelling=page.cancel();cancel.resolve({ok:true,data:snapshot('cancelled',[],null)});await cancelling;
  up.resolve({ok:true,data:snapshot('hybrid_ready',['h1'],'h-next')});await upgrading;
  assert.equal(page.phase,'cancelled');assert.equal(page.results.length,0);
  const paths=[];page.pathStack={pushPathByName:(...args)=>paths.push(args)};
  page.openHit({capture_id:'real-capture',source_id:'source'});
  assert.deepEqual(paths,[['capture','real-capture']]);
}
(async()=>{
  for(const test of [cancelledStartup,recordingErrors,leaveDuringPermission,editorState,searchTransitions]){
    await test();console.log('PASS '+test.name);
  }
})().catch(e=>{console.error(e);process.exitCode=1;});

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
async function llmConfiguration(){
  const calls=[];let saved=null;let secretReceived=null;let failList=false;let failSave=false;let confirmed=true;
  let provider={id:'llm',label:'Existing',adapter_id:'openai_compatible',endpoint:'https://example.com/v1',
    model:'model-a',capabilities:['generate_text'],credential_ref:'sec_existing',enabled:false,test_status:'untested'};
  const local={...provider,id:'asr',adapter_id:'sherpa_onnx',capabilities:['transcribe_audio']};
  const ok=data=>({ok:true,data,error:null});
  const failed={ok:false,data:null,error:{code:'network_unavailable',message:'offline',retryable:true}};
  const gateway={call:async(db,method,params)=>{
    calls.push(method);
    if(method==='providers.list')return failList?failed:ok([provider,local]);
    if(method==='providers.presets')return ok([]);
    if(method==='providers.save'){
      saved=JSON.parse(JSON.stringify(params.config));secretReceived=params.secret;
      if(failSave)return failed;
      provider={...saved,id:'llm',credential_ref:params.secret?'sec_new':saved.credential_ref};return ok(provider);
    }
    if(method==='providers.models')return ok([{id:'detected'}]);
    if(method==='providers.test'){
      assert.deepEqual(Array.from(params.scope),['generate_text']);
      provider={...provider,test_status:'ok'};return ok({message:null,actual_call_made:true});
    }
    if(method==='providers.remove')return ok(null);
    throw new Error('Unexpected '+method);
  }};
  const {LlmConfigPage}=load('pages/LlmConfigPage.ets',{
    '../ui/CoreGateway':{CoreGateway:gateway,uiErrorOf:r=>({...empty,...r.error})},
    '../ui/Errors':errorModule,'../ui/AppRuntime':{confirmAction:async()=>confirmed},
    '../ui/AppState':{AppState:{bumpDataVersion:noop}}},{},true);
  const page=new LlmConfigPage();await page.load();
  assert.deepEqual(calls,['providers.list','providers.presets']);assert.equal(page.providers.length,1);
  page.adopt(provider);page.label='Renamed';page.dirty=true;await page.save();
  assert.equal(saved.credential_ref,'sec_existing');assert.equal(secretReceived,null);assert.equal(saved.enabled,false);
  page.secret='new-test-secret';page.dirty=true;await page.save();
  assert.equal(secretReceived,'new-test-secret');assert.equal(page.secret,'');
  assert.equal(page.selected.credential_ref,'sec_new');
  const before=calls.length;page.model='changed';page.dirty=true;await page.test();await page.detectModels();
  assert.equal(calls.length,before);await page.save();await page.detectModels();assert.equal(page.models[0].id,'detected');
  await page.toggleEnabled();assert.equal(page.selected.enabled,false); // untested cannot enable
  await page.test();assert.equal(page.selected.test_status,'ok');await page.toggleEnabled();
  assert.equal(saved.enabled,true);assert.equal(saved.credential_ref,'sec_new');assert.equal(secretReceived,null);
  failList=true;await page.test();assert.equal(page.canProbe(),false);failList=false;await page.load();assert.equal(page.canProbe(),true);
  page.secret='never-echo-this';page.dirty=true;failSave=true;await page.save();
  assert.equal(page.secret,'');assert.equal(page.canProbe(),false);assert.ok(!page.message.includes('never-echo-this'));failSave=false;
  page.secret='draft-secret';confirmed=false;await page.newProvider(null);assert.equal(page.secret,'draft-secret');
  confirmed=true;await page.newProvider(null);assert.equal(page.secret,'');assert.equal(page.selected,null);
  page.label='Local';page.endpoint='http://localhost:8080/v1';page.model='local';await page.save();assert.equal(saved.endpoint,'http://localhost:8080/v1');
  for(const endpoint of ['http://remote.example/v1','https://example.com/v1/chat/completions','https://user:password@example.com/v1','https://example.com/v1?key=secret']){
    const count=calls.length;page.endpoint=endpoint;page.dirty=true;await page.save();assert.equal(calls.length,count);
  }
  page.secret='temporary';page.aboutToDisappear();assert.equal(page.secret,'');
}
async function isolatedFirstRunUi(){
  for(const debug of [true,false]){
    const removed=[];
    const directory=load('session/AppDirectory.ets',{'BuildProfile':{default:{DEBUG:debug}},
      '@kit.CoreFileKit':{fileIo:{accessSync:()=>true,rmdirSync:p=>removed.push(p)}}});
    directory.selectFirstRunSession('fresh-20261024');
    assert.equal(directory.appFilesDir({filesDir:'/app'}),debug?'/app/first-run-ui/fresh-20261024':'/app');
    if(debug){directory.clearFirstRunSession({filesDir:'/app'});assert.deepEqual(removed,['/app/first-run-ui/fresh-20261024']);}
    else assert.throws(()=>directory.clearFirstRunSession({filesDir:'/app'}),/未选择隔离测试目录/);
    directory.selectFirstRunSession('../library');assert.equal(directory.appFilesDir({filesDir:'/app'}),'/app');
    assert.throws(()=>directory.clearFirstRunSession({filesDir:'/app'}),/未选择隔离测试目录/);
    directory.selectFirstRunSession('');assert.equal(directory.appFilesDir({filesDir:'/app'}),'/app');
  }
}
async function resumedDiaryFeedback(){
  const {DiaryPage}=load('pages/DiaryPage.ets',{'../ui/Errors':errorModule},{},true);
  const page=new DiaryPage();page.waitingConfiguration=true;
  page.acceptedNote='queued';page.runNote='waiting for configuration';
  page.store={refresh:async()=>true,days:[],daysLoaded:true,daysTruncated:false,dayKey:'',day:null,
    dayEmpty:true,currentVersionId:()=>null,candidateVersionIds:()=>[],candidatesOnly:()=>false,
    waitingConfiguration:false,waitingConfigurationCount:0,jobsStatusKnown:true,error:empty,message:'',loading:false,working:false};
  await page.reload();assert.equal(page.waitingConfiguration,false);
  assert.equal(page.acceptedNote,'');assert.equal(page.runNote,'');
  page.waitingConfiguration=true;page.runNote='waiting';page.store.jobsStatusKnown=false;
  await page.reload();assert.equal(page.runNote,'waiting');
}
(async()=>{
  for(const test of [cancelledStartup,recordingErrors,leaveDuringPermission,editorState,searchTransitions,llmConfiguration,isolatedFirstRunUi,resumedDiaryFeedback]){
    await test();console.log('PASS '+test.name);
  }
})().catch(e=>{console.error(e);process.exitCode=1;});

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
      .replace(/@(State|Prop|StorageProp\([^)]*\)|StorageLink\([^)]*\)|Watch\([^)]*\))\s*/g,'');
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
function recordingPageEnv(recover,permission,events){
  const {RecordingPage}=load('pages/RecordingPage.ets',{
    '../ui/Errors':errorModule,
    '../ui/AppRuntime':{databasePath:()=>'/qa/library.sqlite'},
    '../session/AppDirectory':{appFilesDir:()=>'/qa'},
    '../ui/CoreGateway':{CoreGateway:{call:async()=>{events.push('recover');return recover;}}},
    '../recording/MicPermission':{ensureMicrophonePermission:async()=>{events.push('permission');return permission;}},
    '../ui/CoreSession':{CoreSession:{open:()=>{events.push('open');return {core:{}};}}},
    '../recording/VoiceRecorder':{RecorderOptions:class{},VoiceRecorder:class{
      async start(){events.push('start');}async abort(){events.push('abort');}
      receivedMs(){return 0;}persistedMs(){return 0;}
      keepScreenResult(){return {ok:true};}taskResult(){return {ok:true};}
    }}},{getContext:()=>({}),setInterval:()=>1,clearInterval:noop},true);
  return new RecordingPage();
}
async function recordingStartsOnEntry(){
  const recovered={ok:true,data:{recording_id:null}};
  for(const outcome of ['granted','denied','leave-recovery','leave-permission']){
    const recovery=deferred(),permission=deferred(),events=[];
    const page=recordingPageEnv(recovery.promise,permission.promise,events);
    page.aboutToAppear();page.aboutToAppear();
    assert.deepEqual(events,['recover']);
    if(outcome==='leave-recovery')page.aboutToDisappear();
    recovery.resolve(recovered);await new Promise(setImmediate);
    if(outcome==='leave-recovery'){assert.deepEqual(events,['recover']);continue;}
    assert.equal(page.phase,'preparing');assert.deepEqual(events,['recover','permission']);
    if(outcome==='leave-permission')page.aboutToDisappear();
    permission.resolve({granted:outcome!=='denied',message:'需要麦克风权限'});
    await new Promise(setImmediate);
    if(outcome==='granted'){
      assert.equal(page.phase,'recording');await page.startRecording();
      assert.deepEqual(events,['recover','permission','open','start']);page.aboutToDisappear();
    }else{
      assert.ok(!events.includes('open'));
      if(outcome==='denied'){assert.equal(page.phase,'idle');assert.equal(page.permissionDenied,true);}
    }
  }
  const paths=[];
  const {RecordPage}=load('pages/RecordPage.ets',{'../ui/Errors':errorModule},{},true);
  const page=new RecordPage();page.pathStack={getAllPathName:()=>paths.map(x=>x[0]),pushPathByName:(...args)=>paths.push(args)};
  page.openRecording();page.openRecording();page.busy=true;page.openRecording();assert.deepEqual(paths,[['recording','']]);
}
async function recordingAgainStartsImmediately(){
  for(const outcome of ['ready','cleanup-failed','leave']){
    const events=[],gate=deferred();
    const page=recordingPageEnv({ok:true,data:{recording_id:null}},{granted:true},events);
    page.active=true;page.context={};page.phase='saved';page.finalized={capture_id:'previous'};
    page.recorder={abort:async()=>{events.push('cleanup');await gate.promise;if(outcome==='cleanup-failed')throw new Error('cleanup failed');}};
    const pending=page.recordAgain();await page.recordAgain();assert.deepEqual(events,['cleanup']);
    if(outcome==='leave')page.aboutToDisappear();
    gate.resolve();await pending;
    if(outcome==='ready'){
      assert.equal(page.phase,'recording');assert.equal(page.finalized,null);
      assert.deepEqual(events,['cleanup','recover','permission','open','start']);page.aboutToDisappear();
    }else{assert.ok(!events.includes('start'));if(outcome==='cleanup-failed')assert.equal(page.phase,'error');}
  }
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
async function directRecordSave(){
  for(const outcome of ['saved','fragment','not-durable','commit-failed','write-failed']){
    const calls=[];let bumps=0;
    const original=outcome==='fragment'?'  今天，河边……然后  ':'今天在河边散步。';
    const {RecordPage}=load('pages/RecordPage.ets',{
      '../ui/Errors':errorModule,'../ui/AppState':{AppState:{bumpDataVersion:()=>bumps++}},
      '../recording/VoiceRecorder':{readTimeZone:()=> 'Asia/Shanghai'},
      '../ui/AppRuntime':{newOperationId:x=>x},
      '../ui/CoreGateway':{uiErrorOf:r=>r.error,CoreGateway:{call:async(db,method,params)=>{
        calls.push(method);
        if(method==='captures.createDraft')return {ok:true,data:{id:'capture',revision:3}};
        if(method==='captures.saveDraft'){
          assert.equal(params.capture_id,'capture');assert.equal(params.expected_revision,3);
          assert.equal(params.text,original);
          return outcome==='write-failed'?{ok:false,error:{code:'storage_full'}}:
            {ok:true,data:{revision:4,durable:outcome!=='not-durable',saved_at:'2026-10-24T05:30:00Z'}};
        }
        if(method==='captures.commit'){
          assert.equal(params.capture_id,'capture');assert.equal(params.expected_revision,4);
          return outcome==='commit-failed'?{ok:false,error:{code:'storage_full'}}:
            {ok:true,data:{capture:{id:'capture',revision:5,state:'committed'}}};
        }
        throw new Error('Unexpected '+method);
      }}}},{},true);
    const page=new RecordPage();page.text=original;
    await page.save();
    assert.equal(page.busy,false);assert.equal(page.text,original);
    assert.deepEqual(calls,['saved','fragment','commit-failed'].includes(outcome)
      ?['captures.createDraft','captures.saveDraft','captures.commit']
      :['captures.createDraft','captures.saveDraft']);
    if(outcome==='saved'||outcome==='fragment'){
      assert.equal(page.savedState,'committed');assert.equal(page.revision,5);
      assert.equal(page.stateLabel(),'已保存');assert.equal(page.canSave(),false);assert.equal(bumps,1);
      await page.save();assert.equal(calls.length,3);
    }else{
      assert.notEqual(page.savedState,'committed');
      if(outcome==='not-durable'){assert.equal(page.savedAt,'');assert.match(page.message,/尚未确认/);}
      else assert.equal(page.error.code,'storage_full');
      if(outcome==='commit-failed')assert.match(page.message,/内容已保留/);
    }
  }
}
function snapshot(phase,results,cursor){
  return {session_id:'s',phase,results:results.map(x=>typeof x==='string'?searchHit(x):x),cursor,index_coverage:'complete',warnings:[],query_revision:1};
}
function searchHit(id,fields={}){
  return {hit_id:id,group_id:'source-'+id,capture_id:'capture-'+id,source_id:'source-'+id,
    source_kind:'text',matched_by:['keyword'],coverage:'complete',revision_id:null,
    day_key:'2026-10-24',title:null,snippet:id,highlights:[],locator:null,...fields};
}
async function searchTransitions(){
  const next=deferred(),up=deferred(),cancel=deferred(),calls=[];
  const {SearchPage}=load('pages/SearchPage.ets',{'../ui/Errors':errorModule,
    '../ui/CoreGateway':{CoreGateway:{call:(db,method)=>{calls.push(method);
      return method==='search.nextPage'?next.promise:method==='search.upgrade'?up.promise:cancel.promise;}}}},{},true);
  const page=new SearchPage();page.sessionId='s';page.hasMore=true;page.cursor='k-cursor';page.results=[searchHit('k1')];
  const paging=page.loadMore();await page.upgrade();assert.deepEqual(calls,['search.nextPage']);
  next.resolve({ok:true,data:snapshot('keyword_ready',['k2'],'k-next')});await paging;
  const upgrading=page.upgrade();await page.loadMore();
  assert.deepEqual(calls,['search.nextPage','search.upgrade']);
  const cancelling=page.cancel();cancel.resolve({ok:true,data:snapshot('cancelled',[],null)});await cancelling;
  up.resolve({ok:true,data:snapshot('hybrid_ready',['h1'],'h-next')});await upgrading;
  assert.equal(page.phase,'cancelled');assert.equal(page.results.length,2);
  const paths=[];page.pathStack={pushPathByName:(...args)=>paths.push(args)};
  page.openHit({capture_id:'real-capture',source_id:'source'});
  assert.deepEqual(paths,[['capture','real-capture']]);
}
async function automaticHybridSearch(){
  for(const outcome of ['fused','unavailable','failed','keyword','semantic']){
    const up=deferred(),entered=deferred(),calls=[];
    const {SearchPage}=load('pages/SearchPage.ets',{'../ui/Errors':errorModule,
      '../ui/CoreGateway':{uiErrorOf:r=>r.error,CoreGateway:{call:async(db,method,params)=>{
        calls.push(method);
        if(method==='indexes.status')return {ok:true,data:{semantic_index_ready:true}};
        if(method==='search.start'){
          assert.equal(params.request.mode,outcome==='keyword'||outcome==='semantic'?outcome:'hybrid');
          return {ok:true,data:snapshot(outcome==='semantic'?'done':'keyword_ready',['k1'],'k-cursor')};
        }
        if(method==='search.upgrade'){entered.resolve();return up.promise;}
        throw new Error('Unexpected '+method);
      }}}},{},true);
    const page=new SearchPage();page.query='散步';
    page.mode=outcome==='keyword'||outcome==='semantic'?outcome:'hybrid';
    const searching=page.startSearch();
    if(page.mode!=='hybrid'){
      await searching;assert.ok(!calls.includes('search.upgrade'));continue;
    }
    await entered.promise;
    assert.equal(page.upgrading,true);assert.equal(page.resultStatus(),'正在结合语义搜索…');
    assert.equal(page.results[0].hit_id,'k1');await page.loadMore();assert.ok(!calls.includes('search.nextPage'));
    up.resolve(outcome==='failed'?{ok:false,error:{code:'model_unavailable'}}:
      {ok:true,data:outcome==='fused'?snapshot('hybrid_ready',['h1'],'h-cursor'):snapshot('keyword_ready',['k1'],'k-cursor')});
    await searching;
    assert.equal(calls.filter(x=>x==='search.upgrade').length,1);
    assert.equal(page.upgrading,false);assert.equal(page.results[0].hit_id,outcome==='fused'?'h1':'k1');
    assert.equal(page.cursor,outcome==='fused'?'h-cursor':'k-cursor');
    if(outcome!=='fused')assert.equal(page.resultStatus(),'当前仅有关键词结果');
    if(outcome==='failed')assert.equal(page.error.code,'model_unavailable');
  }
}
async function hybridQueryReplacement(){
  const up=deferred(),entered=deferred(),cancelled=[];
  const {SearchPage}=load('pages/SearchPage.ets',{'../ui/Errors':errorModule,
    '../ui/CoreGateway':{CoreGateway:{call:async(db,method,params)=>{
      if(method==='indexes.status')return {ok:true,data:{semantic_index_ready:true}};
      if(method==='search.start')return {ok:true,data:{...snapshot('keyword_ready',[params.request.query],null),session_id:params.request.query}};
      if(method==='search.upgrade'){entered.resolve();return up.promise;}
      if(method==='search.cancel'){cancelled.push(params.session_id);return {ok:true,data:snapshot('cancelled',[],null)};}
      throw new Error('Unexpected '+method);
    }}}},{},true);
  const page=new SearchPage();page.mode='hybrid';page.query='first';
  const previous=page.startSearch();await entered.promise;
  page.mode='keyword';page.query='second';await page.startSearch();
  up.resolve({ok:true,data:snapshot('hybrid_ready',['old-result'],null)});await previous;
  assert.deepEqual(cancelled,['first']);assert.equal(page.sessionId,'second');
  assert.equal(page.results[0].hit_id,'second');assert.equal(page.phase,'keyword_ready');
  assert.equal(page.upgrading,false);
}
function searchPageEnv(gateway){
  const {SearchPage}=load('pages/SearchPage.ets',{'../ui/Errors':errorModule,
    '../ui/AppRuntime':{databasePath:()=>'/qa/library.sqlite',searchPhaseLabel:x=>x},
    '../ui/CoreGateway':{uiErrorOf:r=>r.error,CoreGateway:{call:async(db,method,params)=>{
      if(method==='indexes.status')return {ok:true,data:{semantic_index_ready:true}};
      return gateway(method,params);
    }}}},{getContext:()=>({})},true);
  return new SearchPage();
}
async function searchCancelsPendingRequest(){
  const pending=deferred(),cancelled=[];
  const page=searchPageEnv(async(method,params)=>{
    if(method==='search.start')return pending.promise;
    if(method==='search.cancel'){cancelled.push(params.session_id);return {ok:true};}
  });
  page.query='旧查询';const searching=page.startSearch();await new Promise(setImmediate);
  await page.cancel();assert.equal(page.loading,false);assert.equal(page.phase,'cancelled');
  pending.resolve({ok:true,data:snapshot('done',['late'],null)});await searching;
  assert.equal(page.results.length,0);assert.deepEqual(cancelled,['s']);
}
async function searchEmptyQueryInvalidatesOld(){
  const pending=deferred(),cancelled=[];
  const page=searchPageEnv(async(method,params)=>{
    if(method==='search.start')return pending.promise;
    if(method==='search.cancel'){cancelled.push(params.session_id);return {ok:true};}
  });
  page.query='旧查询';const searching=page.startSearch();await new Promise(setImmediate);
  page.query='';await page.startSearch();
  pending.resolve({ok:true,data:snapshot('done',['old'],null)});await searching;
  assert.equal(page.results.length,0);assert.equal(page.sessionId,'');assert.equal(page.loading,false);
  assert.deepEqual(cancelled,['s']);
}
async function searchPaginationRetryClearsError(){
  let attempt=0;
  const page=searchPageEnv(async()=>++attempt===1?{ok:false,error:{code:'network_unavailable'}}:
    {ok:true,data:snapshot('done',['next'],null)});
  page.sessionId='s';page.hasMore=true;page.cursor='cursor';page.results=[searchHit('first')];
  await page.loadMore();assert.notEqual(page.error.code,'');
  await page.loadMore();assert.equal(page.error.code,'');assert.equal(page.results.length,2);
}
async function searchExpiredCursorRestarts(){
  for(const code of ['search_expired','cursor_expired']){
    let starts=0;const page=searchPageEnv(async method=>{
      if(method==='search.start')return {ok:true,data:snapshot('keyword_ready',[++starts===1?'old':'fresh'],'next')};
      if(method==='search.nextPage')return {ok:false,error:{code}};
      return {ok:true};
    });
    page.query='散步';await page.startSearch();await page.loadMore();
    assert.equal(starts,2,code);assert.equal(page.results[0].hit_id,'fresh');assert.equal(page.error.code,'');
  }
}
async function searchResultsDoNotDuplicateOriginal(){
  const same={capture_id:'capture',snippet:'今天在河边散步。'};
  const capture=searchHit('capture',{...same,source_id:null});
  const source=searchHit('source',{...same,matched_by:['semantic']});
  const differentSource=searchHit('different-source',same);
  const page=searchPageEnv(async()=>({ok:true}));
  page.applySnapshot(snapshot('keyword_ready',[capture,source],'next'),false);
  assert.equal(page.results.length,1);assert.equal(page.results[0].source_id,source.source_id);
  assert.deepEqual(Array.from(page.results[0].matched_by).sort(),['keyword','semantic']);
  page.applySnapshot(snapshot('done',[source,differentSource],null),true);
  assert.equal(page.results.length,2,'different original sources must remain available');
}
async function searchEditsDiscardStaleResults(){
  const pending=deferred(),cancelled=[];
  const page=searchPageEnv(async(method,params)=>{
    if(method==='search.start')return pending.promise;
    if(method==='search.cancel'){cancelled.push(params.session_id);return {ok:true};}
  });
  page.query='旧查询';const searching=page.startSearch();await new Promise(setImmediate);
  page.updateQuery('新查询');
  pending.resolve({ok:true,data:snapshot('done',['old'],null)});await searching;
  assert.equal(page.query,'新查询');assert.equal(page.results.length,0);assert.equal(page.searched,false);
  assert.deepEqual(cancelled,['s']);
  page.error={code:'invalid_argument'};page.updateQuery('再换一个词');assert.equal(page.error.code,'');
}
async function searchRefreshesAfterDataChanges(){
  let starts=0;const page=searchPageEnv(async method=>{
    if(method==='search.start')return {ok:true,data:snapshot('done',['version-'+(++starts)],null)};
    return {ok:true};
  });
  page.aboutToAppear();page.query='散步';await page.startSearch();
  page.visible=false;page.onVisibilityChanged();page.onDataVersionChanged();
  await new Promise(setImmediate);assert.equal(starts,1);assert.equal(page.results.length,0);
  page.visible=true;page.onVisibilityChanged();await new Promise(setImmediate);
  assert.equal(starts,2);assert.equal(page.results[0].hit_id,'version-2');
  page.onDataVersionChanged();await new Promise(setImmediate);
  assert.equal(starts,3);assert.equal(page.results[0].hit_id,'version-3');
  await page.cancel();page.onDataVersionChanged();await new Promise(setImmediate);
  assert.equal(starts,3);assert.equal(page.results.length,0,'cancelled cached snippets must not survive data changes');
  page.aboutToDisappear();
}
async function searchHybridDoesNotReportEmptyWhilePending(){
  const upgrade=deferred(),entered=deferred();
  const page=searchPageEnv(async method=>{
    if(method==='search.start')return {ok:true,data:snapshot('keyword_ready',[],null)};
    if(method==='search.upgrade'){entered.resolve();return upgrade.promise;}
    return {ok:true};
  });
  page.mode='hybrid';page.query='沿河漫步';const searching=page.startSearch();await entered.promise;
  assert.equal(page.waitingForResults(),true);
  upgrade.resolve({ok:true,data:snapshot('done',['semantic'],null)});await searching;
  assert.equal(page.waitingForResults(),false);assert.equal(page.results[0].hit_id,'semantic');
}
async function searchUpgradeExpiryRetryIsBounded(){
  let starts=0,upgrades=0;
  const page=searchPageEnv(async method=>{
    if(method==='search.start')return {ok:true,data:{...snapshot('keyword_ready',['keyword'],null),session_id:'session-'+(++starts)}};
    if(method==='search.upgrade'){upgrades++;return {ok:false,error:{code:'search_expired'}};}
    return {ok:true};
  });
  page.query='散步';page.mode='hybrid';await page.startSearch();
  assert.equal(starts,2);assert.equal(upgrades,2);assert.equal(page.error.code,'search_expired');
  assert.equal(page.upgrading,false);assert.equal(page.results[0].hit_id,'keyword');
}
async function searchCancellationKeepsLoadedPages(){
  const page=searchPageEnv(async()=>({ok:true,data:snapshot('cancelled',['last-page'],null)}));
  page.applySnapshot(snapshot('keyword_ready',['first-page'],'next'),false);
  page.applySnapshot(snapshot('keyword_ready',['second-page'],'next2'),true);
  await page.cancel();
  assert.deepEqual(Array.from(page.results,x=>x.hit_id),['first-page','second-page']);
  assert.equal(page.hasMore,false);assert.equal(page.phase,'cancelled');
}
async function searchIndexStatusIgnoresStaleResponse(){
  const older=deferred();let requests=0;
  const {SearchPage}=load('pages/SearchPage.ets',{'../ui/Errors':errorModule,
    '../ui/CoreGateway':{CoreGateway:{call:async()=>++requests===1?older.promise:
      {ok:true,data:{semantic_index_ready:true}}}}},{},true);
  const page=new SearchPage();page.ready=true;
  const first=page.loadIndexStatus();await page.loadIndexStatus();
  older.resolve({ok:true,data:{semantic_index_ready:false}});await first;
  assert.equal(page.indexStatus.semantic_index_ready,true);
}
async function llmConfiguration(){
  const calls=[];let saved=null;let secretReceived=null;let failList=false;let failSave=false;let failTest=false;let confirmed=true;
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
      if(failTest)return {ok:false,data:null,error:{code:'authentication_failed',message:'HTTP 401',retryable:false}};
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
  failTest=true;await page.test();assert.equal(page.error.code,'authentication_failed');assert.equal(page.message,'');failTest=false;
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
  page.store.jobsStatusKnown=true;
  page.store.generate=async()=>({job_id:'job'});
  page.store.runDueJobs=async()=>{page.store.waitingConfiguration=true;page.store.message='请先配置模型服务。';};
  page.store.openDay=async()=>true;
  await page.generateAndRun();assert.equal(page.waitingConfiguration,true);
  assert.equal(page.acceptedNote,'');assert.match(page.runNote,/配置/);
}
async function chineseSystemFeedback(){
  const {toUiError}=load('ui/Errors.ets',{'../core/DiaryError':{DiaryError}});
  for(const code of ['authentication_failed','network_unavailable','storage_full','model_unavailable','native_unavailable','future_service_error']){
    const result=toUiError({code,message:'HTTP 401 at /private/library.sqlite: secret_value',retryable:true});
    assert.equal(result.code,code);assert.equal(result.retryable,true);
    assert.ok(!/[A-Za-z/]/.test(result.title+result.message+result.hint));
    assert.ok((result.title+result.message+result.hint).length<50);
  }
  const runtime=load('ui/AppRuntime.ets',{});
  for(const name of ['captureStateLabel','jobStateLabel','jobKindLabel','extractStatusLabel','coverageLabel','diaryOriginLabel','hitSourceKindLabel','matchedByLabel','searchPhaseLabel']){
    assert.ok(!/[A-Za-z]/.test(runtime[name]('unrecognized_future_value')),name);
  }
  assert.notEqual(runtime.jobKindLabel('diary_generate'),runtime.jobKindLabel('unrecognized'));
  const text=load('ui/UserText.ets',{'./Errors':{toUiError}});
  assert.ok(!/[A-Za-z]/.test(text.presetLabel('DeepSeek')+text.modelLabel('deepseek-flash')+text.testStatusLabel('ok')));
  assert.ok(!/[A-Za-z]/.test(text.timeZoneLabel('UTC')+text.timeZoneLabel('Asia/Shanghai')));
}
async function exportFeedbackHidesPaths(){
  const ticket={dayCount:1,byteSize:123,path:'/private/exports/diary-20261024.md',packageTicket:'/private/exports/diary-20261024.md',
    format:'markdown',dayKeys:['2026-10-24'],restorable:false};
  const {ExportStore}=load('state/ExportStore.ets',{'../ui/Errors':errorModule,
    '../ui/CoreGateway':{CoreGateway:{call:async()=>({ok:true,data:ticket})}}});
  const store=new ExportStore('/private/library.sqlite');
  assert.equal(await store.createExport({fromDayKey:'2026-10-24',toDayKey:'2026-10-24'}),ticket);
  assert.equal(store.ticket.path,ticket.path);
  assert.ok(!/[A-Za-z/]/.test(store.message));assert.ok(store.message.length<50);
}
async function styleProfilesStayIndependent(){
  let nextId=0,activeId='sty_default';
  const records=new Map([['sty_default',{id:'sty_default',label:'默认风格',revision:1,
    lengthPreference:null,person:null,tone:null,emotionalTemperature:null,rhetoricLevel:null,
    examples:[],bannedExpressions:[]}]]);
  const store={styles:Array.from(records.values()),error:empty,message:'',conflicted:false,current:null,
    refresh:async()=>{store.styles=Array.from(records.values());},
    saveStyle:async(input,revision)=>{
      const old=records.get(input.id);
      assert.equal(revision,old?old.revision:0);
      const saved={...input,revision:revision+1};records.set(saved.id,saved);
      store.styles=Array.from(records.values());store.message='已保存';return saved;
    }};
  const settings={settings:{default_style_id:null},error:empty,message:'',
    refresh:async()=>{},setDefaultStyle:async id=>{
      assert.ok(records.has(id));activeId=id;settings.settings={default_style_id:id};return true;
    }};
  const {StylesPage}=load('pages/StylesPage.ets',{
    '../ui/Errors':errorModule,
    '../state/StyleStore':{DEFAULT_STYLE_ID:'sty_default'},
    '../ui/AppRuntime':{newOperationId:()=>`style-${++nextId}`},
    '../ui/AppState':{AppState:{bumpDataVersion:noop}}},{},true);
  const page=new StylesPage();page.store=store;page.settings=settings;await page.load();
  assert.equal(page.selectedId,'sty_default');
  page.createStyle();page.label='日常随记';page.applyPreset(1);page.tone='轻柔一点';page.toneCustom=true;
  await page.save();const first=page.selectedId;
  assert.equal(records.get(first).tone,'轻柔一点');assert.equal(activeId,'sty_default');
  page.createStyle();page.label='周末小记';page.applyPreset(2);await page.saveAndUse();
  const second=page.selectedId;assert.notEqual(first,second);assert.equal(activeId,second);
  assert.equal(records.get(first).tone,'轻柔一点');
  page.editStyle(records.get(first));page.label='日常随记修改';page.temperature='含蓄一些';
  page.editStyle(records.get(second));assert.equal(page.label,'周末小记');
  page.editStyle(records.get(first));assert.equal(page.label,'日常随记修改');
  await page.save();assert.equal(records.get(first).emotionalTemperature,'含蓄一些');
  assert.equal(records.get(second).label,'周末小记');assert.equal(activeId,second);
  await page.useStyle(first);assert.equal(activeId,first);
  assert.equal(records.get(second).tone,'平实书面');
}
async function scheduleSettingsCanChange(){
  const updates=[];let dialog=null;
  let saved={auto_organize_enabled:false,auto_organize_time:'22:30',
    diary_time_zone:'UTC',default_style_id:'sty_custom',revision:1};
  const gateway={call:async(db,method,params)=>{
    if(method==='settings.get')return {ok:true,data:{...saved}};
    assert.equal(method,'settings.update');assert.equal(params.expected_revision,saved.revision);
    assert.equal(Object.hasOwn(params.settings,'default_style_id'),false);
    const input=params.settings;
    if(input.auto_organize_time!==saved.auto_organize_time)updates.push(['time',input.auto_organize_time]);
    if(input.diary_time_zone!==saved.diary_time_zone)updates.push(['zone',input.diary_time_zone]);
    if(input.auto_organize_enabled!==saved.auto_organize_enabled)updates.push(['enabled',input.auto_organize_enabled]);
    saved={...saved,...input,revision:saved.revision+1};return {ok:true,data:{...saved}};
  }};
  const {SettingsStore}=load('state/SettingsStore.ets',{
    '../ui/CoreGateway':{CoreGateway:gateway,uiErrorOf:r=>r.error},
    '../ui/Errors':errorModule});
  const store=new SettingsStore('db');
  const {SchedulePage}=load('pages/SchedulePage.ets',{
    '../ui/Errors':errorModule,
    '../state/SettingsStore':{SettingsStore},
    '../ui/AppState':{AppState:{bumpDataVersion:noop}}},
    {TimePickerDialog:{show:options=>{dialog=options;}}},true);
  const page=new SchedulePage();page.store=store;await page.load();
  page.pickTime();assert.equal(dialog.useMilitaryTime,true);
  dialog.onAccept({hour:8,minute:5});await new Promise(setImmediate);
  await page.saveZone('Asia/Shanghai');await page.toggleAutoOrganize(true);
  assert.deepEqual(updates,[['time','08:05'],['zone','Asia/Shanghai'],['enabled',true]]);
  assert.equal(page.settings.auto_organize_time,'08:05');assert.equal(saved.default_style_id,'sty_custom');
  assert.equal(page.zoneLabel('Asia/Shanghai'),'中国标准时间');
  await page.saveZone('Asia/Shanghai');assert.equal(updates.length,3);
}
(async()=>{
  const failed=[];
  for(const test of [cancelledStartup,recordingErrors,leaveDuringPermission,recordingStartsOnEntry,recordingAgainStartsImmediately,editorState,directRecordSave,searchTransitions,automaticHybridSearch,hybridQueryReplacement,searchCancelsPendingRequest,searchEmptyQueryInvalidatesOld,searchPaginationRetryClearsError,searchExpiredCursorRestarts,searchResultsDoNotDuplicateOriginal,searchEditsDiscardStaleResults,searchRefreshesAfterDataChanges,searchHybridDoesNotReportEmptyWhilePending,searchUpgradeExpiryRetryIsBounded,searchCancellationKeepsLoadedPages,searchIndexStatusIgnoresStaleResponse,llmConfiguration,isolatedFirstRunUi,resumedDiaryFeedback,chineseSystemFeedback,exportFeedbackHidesPaths,styleProfilesStayIndependent,scheduleSettingsCanChange]){
    try{await test();console.log('PASS '+test.name);}catch(e){failed.push(test.name);console.error('FAIL '+test.name+': '+e.message);}
  }
  assert.equal(failed.length,0,failed.join(', '));
})().catch(e=>{console.error(e);process.exitCode=1;});

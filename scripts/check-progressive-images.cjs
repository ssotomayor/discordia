const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),assert=require('node:assert/strict'),{spawn,spawnSync}=require('node:child_process');
const root=path.resolve(__dirname,'..'),out=path.join(root,'target/unified-images'),exe=path.join(out,'Progressive.exe');
fs.mkdirSync(path.join(root,'target/image-reports'),{recursive:true});
const sleep = ms => new Promise(r => setTimeout(r, ms));
let id = 0;
let channel;
async function connect(url) {
  const ws = new WebSocket(url), pending = new Map();
  await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject;});
  channel={url,ws,pending};
  ws.onmessage=event=>{
    const message=JSON.parse(event.data), request=pending.get(message.id);
    if(!request)return;
    pending.delete(message.id);clearTimeout(request.timer);
    if(message.error||message.result?.exceptionDetails)request.reject(new Error(JSON.stringify(message)));
    else request.resolve(message.result);
  };
}
async function rpc(url, method, params = {}) {
  if(channel?.url===url) return new Promise((resolve,reject)=>{
    const call=++id,timer=setTimeout(()=>{channel.pending.delete(call);reject(new Error(method+' timeout'));},30000);
    channel.pending.set(call,{resolve,reject,timer});channel.ws.send(JSON.stringify({id:call,method,params}));
  });
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url), call = ++id;
    const timer = setTimeout(() => {ws.close(); reject(new Error(method + ' timeout'));}, 30000);
    ws.onopen = () => ws.send(JSON.stringify({id:call,method,params}));
    ws.onerror = () => {clearTimeout(timer); reject(new Error('CDP connection'));};
    ws.onmessage = event => {const m=JSON.parse(event.data); if(m.id!==call)return; clearTimeout(timer); ws.close(); if(m.error||m.result?.exceptionDetails)reject(new Error(JSON.stringify(m))); else resolve(m.result);};
  });
}
async function evaluate(url, expression) { return (await rpc(url,'Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true})).result?.value; }
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

(async()=>{
 const runs=[];
 for(let run=1;run<=5;run++){
  const folder=path.join(out,'viewer-check-'+Date.now());fs.mkdirSync(folder);
  const child=spawn(exe,[],{cwd:root,env:{...process.env,IMAGE_PROBE_DATA:path.join(out,'fixtures/jpeg-large/server'),IMAGE_PROBE_ORIGINAL_DATA:path.join(out,'fixtures/jpeg-large/server-original'),WEBVIEW2_USER_DATA_FOLDER:path.join(folder,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-address=127.0.0.1 --remote-debugging-port=19349'},stdio:'ignore'});fs.writeFileSync(path.join(folder,'owner.json'),JSON.stringify({pid:child.pid,exe}));
  try{
   let url;
   for(let n=0;n<200;n++){try{const tabs=await(await fetch('http://127.0.0.1:19349/json/list')).json();const p=tabs.find(t=>t.type==='page'&&t.url.includes('dioxus'));if(p&&await evaluate(p.webSocketDebuggerUrl,'!!document.querySelector("#viewer-open")')){url=p.webSocketDebuggerUrl;break;}}catch{}await sleep(100);}
   assert.ok(url,'native viewer fixture ready');await connect(url);
   const wait=async expression=>{for(let n=0;n<300;n++){if(await evaluate(url,expression))return;await sleep(30);}throw Error('Timed out: '+expression);};
   const click=selector=>evaluate(url,'document.querySelector('+JSON.stringify(selector)+').click()');
   const close=()=>evaluate(url,'document.dispatchEvent(new KeyboardEvent("keydown",{key:"Escape",bubbles:true}))');
   const preview='document.querySelector("#dxf-image-viewer img")?.naturalWidth===640';
   const ready='document.querySelector("#dxf-image-viewer img")?.naturalWidth===4000&&!document.querySelector(".dxf-image-status")';
   await click('#viewer-open');await wait(preview);await wait('document.querySelector(".dxf-image-status")?.textContent.includes("Loading")');
   await evaluate(url,'window.savedViewer=document.getElementById("dxf-image-viewer");window.savedBridge=window.__dxfImageViewerOff');
   await click('[data-viewer-action=in]');await click('[data-viewer-action=in]');await evaluate(url,'document.dispatchEvent(new KeyboardEvent("keydown",{key:"ArrowRight",bubbles:true}))');await sleep(80);
   const geometry=await evaluate(url,'({zoom:savedViewer.dataset.zoom,transform:savedViewer.querySelector("img").style.transform,width:savedViewer.querySelector("img").clientWidth,height:savedViewer.querySelector("img").clientHeight})');assert.ok(Number(geometry.zoom)>1);
   await click('#viewer-deliver');await wait(ready);
   const upgraded=await evaluate(url,'({zoom:savedViewer.dataset.zoom,transform:savedViewer.querySelector("img").style.transform,width:savedViewer.querySelector("img").clientWidth,height:savedViewer.querySelector("img").clientHeight})');assert.deepEqual(upgraded,geometry);assert.equal(await evaluate(url,'savedViewer===document.getElementById("dxf-image-viewer")&&savedBridge===window.__dxfImageViewerOff'),true);
   await close();await wait('!document.getElementById("dxf-image-viewer")');
   await click('#viewer-open');await wait(preview);await close();await wait('!document.getElementById("dxf-image-viewer")');await click('#viewer-deliver');await sleep(200);assert.equal(await evaluate(url,'!!document.getElementById("dxf-image-viewer")'),false);
   await click('#viewer-open');await wait(preview);await click('#viewer-fail');await wait('document.querySelector(".dxf-image-status button")');assert.equal(await evaluate(url,preview),true);await click('.dxf-image-status button');await wait('document.querySelector(".dxf-image-status")?.textContent.includes("Loading")');await click('#viewer-deliver');await wait(ready);await close();await wait('!document.getElementById("dxf-image-viewer")');
   await click('#viewer-open');await wait(preview);await click('#viewer-corrupt');await wait('document.querySelector(".dxf-image-status button")');assert.equal(await evaluate(url,preview),true);await click('.dxf-image-status button');await click('#viewer-deliver');await wait(ready);await close();await wait('!document.getElementById("dxf-image-viewer")');
   await sleep(150);const cache=await evaluate(url,"fetch('/probe-stats/current').then(r=>r.json())");assert.equal(cache.encodedBytes,0);
   runs.push({run,passed:['preview visible before original delivery','loading status','upgrade preserves zoom pan geometry and DOM instance','close ignores late delivery','network failure keeps preview and retry works','browser image failure keeps preview and retry works','close releases binary buffers'],environment:await evaluate(url,'({dpr:devicePixelRatio,visibility:document.visibilityState})')});console.log('Native progressive viewer run',run,'passed');
  }finally{
   channel?.ws.close();channel=null;
   const cleanup=spawnSync('powershell.exe',['-NoProfile','-Command',"$p=Get-Process -Id "+child.pid+" -ErrorAction SilentlyContinue;if($p -and $p.Path -eq '"+exe.replaceAll("'","''")+"'){[void]$p.CloseMainWindow();if(-not $p.WaitForExit(5000)){taskkill /PID "+child.pid+" /T /F}};'closed'"],{encoding:'utf8',windowsHide:true});fs.writeFileSync(path.join(folder,'cleanup.log'),cleanup.stdout+cleanup.stderr);await sleep(1500);if(child.exitCode===null)throw Error('Owned native probe remains');
  }
 }
 fs.writeFileSync(path.join(root,'target/image-reports/PROGRESSIVE_IMAGE_CHECKS.json'),JSON.stringify({runs,exeSha256:hash(fs.readFileSync(exe)),manualTests:false,scope:'Production ImageViewer/ImageAssets/JS in real WebView2 with controlled transport; real gateway and DM crypto covered separately'},null,2)+'\n');
})().catch(error=>{console.error(error);process.exitCode=1});

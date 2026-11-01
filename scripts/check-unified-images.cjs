const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),assert=require('node:assert/strict'),{spawn,spawnSync}=require('node:child_process');
const root=path.resolve(__dirname,'..'),out=path.join(root,'target/unified-images'),exe=path.join(out,'Candidate.exe');
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
 const folder=path.join(out,'checks-'+Date.now());fs.mkdirSync(folder);
 const child=spawn(exe,[],{cwd:root,env:{...process.env,IMAGE_PROBE_DATA:path.join(out,'qa/preview'),IMAGE_PROBE_ORIGINAL_DATA:path.join(out,'qa/original'),WEBVIEW2_USER_DATA_FOLDER:path.join(folder,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-address=127.0.0.1 --remote-debugging-port=19349'},stdio:'ignore'});
 const passed=[];
 try{
  let url;
  for(let n=0;n<200;n++){try{const tabs=await(await fetch('http://127.0.0.1:19349/json/list')).json();const tab=tabs.find(t=>t.type==='page'&&t.url.includes('dioxus'));if(tab&&await evaluate(tab.webSocketDebuggerUrl,'!!document.querySelector("#probe")')){url=tab.webSocketDebuggerUrl;break;}}catch{}await sleep(100);}
  assert.ok(url,'native UI ready');await connect(url);
  const env=await evaluate(url,'({dpr:devicePixelRatio,visibility:document.visibilityState,focus:document.hasFocus()})');assert.equal(env.dpr,1);assert.equal(env.visibility,'visible');
  const loaded=async expected=>{for(let n=0;n<200;n++){const rows=await evaluate(url,'[...document.querySelectorAll("#images img")].map(i=>({src:i.currentSrc,w:i.naturalWidth,h:i.naturalHeight,complete:i.complete,width:i.width,height:i.height}))');if(rows.length===expected&&rows.every(i=>i.complete&&i.w>0))return rows;await sleep(50);}throw Error('images did not become ready');};
  const scene=async(n,count)=>{await evaluate(url,"document.querySelector('#scenario-"+n+"').click()");await sleep(80);return loaded(count);};
  const inline=await scene(2,8);assert.equal(inline[0].w,640);assert.equal(inline[0].h,480);passed.push('Native 1x chat loads 640x480 preview');
  const originalResources=await evaluate(url,'performance.getEntriesByType("resource").filter(r=>/image-data/.test(r.name)).length');assert.equal(originalResources,8);passed.push('No additional speculative original asset requests at 1x');
  await rpc(url,'Emulation.setDeviceMetricsOverride',{width:1441,height:900,deviceScaleFactor:3,mobile:false});
  console.log('listener',await evaluate(url,'({off:typeof window.__dxfImageDensityOff,density:document.querySelector("#probe").dataset.density})'));await evaluate(url,'window.dispatchEvent(new Event("resize"))');await sleep(200);
  let dense;for(let n=0;n<200;n++){dense=await loaded(8);if(dense[0].w===4000)break;await sleep(50);}console.log(await evaluate(url, '({dpr:devicePixelRatio,matches:matchMedia("(max-resolution: 2dppx)").matches,density:document.querySelector("#probe").dataset.density})'));assert.equal(dense[0].w,4000);assert.equal(dense[0].h,3000);passed.push('Live 3x density change selects original without remount');
  const getBytes=async src=>Buffer.from(await evaluate(url,'(async()=>{const a=new Uint8Array(await(await fetch('+JSON.stringify(src)+')).arrayBuffer());let s="";for(let n=0;n<a.length;n+=32768)s+=String.fromCharCode(...a.subarray(n,n+32768));return btoa(s)})()'),'base64');
  assert.equal(hash(await getBytes(dense[0].src)),hash(fs.readFileSync(path.join(out,'qa/original/high0.jpg'))));passed.push('Original SHA-256 unchanged');
  await rpc(url,'Emulation.clearDeviceMetricsOverride');let low;for(let n=0;n<200;n++){low=await loaded(8);if(low[0].w===640)break;await sleep(50);}assert.equal(low[0].w,640);passed.push('Live return to 1x restores preview');
  const full=await scene(7,1);assert.equal(full[0].w,4000);for(let n=1;n<=6;n++){await evaluate(url,"document.querySelector('#next').click()");await sleep(60);await loaded(n%2?0:1);}passed.push('Six full-view open/close transitions use original');
  const avatars=await scene(3,36);assert.equal(avatars[0].w,1024);assert.equal(avatars[0].width,32);passed.push('Avatar source and 32px display unchanged');
  const oriented=await scene(10,1);assert.equal(oriented[0].w,320);assert.equal(oriented[0].h,480);passed.push('EXIF preview is physically rotated portrait');
  const alpha=await scene(11,1);const rgba=await evaluate(url,'(()=>{const i=document.querySelector("#images img"),c=document.createElement("canvas");c.width=i.naturalWidth;c.height=i.naturalHeight;const x=c.getContext("2d");x.drawImage(i,0,0);return [...x.getImageData(50,50,1,1).data]})()');assert.equal(rgba[3],96);passed.push('PNG alpha preserved in native rendering');
  await scene(4,4);await evaluate(url,"window.dispatchEvent(new Event('focus'))");await sleep(200);
  const frame=async()=>{const clip=await evaluate(url,'(()=>{const r=document.querySelectorAll("#images img")[3].getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,scale:1}})()');return(await rpc(url,'Page.captureScreenshot',{format:'png',clip})).data;};
  const first=await frame();await sleep(240);assert.notEqual(first,await frame());passed.push('Large GIF advances animated frames');
  const gifSrc=await evaluate(url,'performance.getEntriesByType("resource").filter(r=>/gif$/.test(r.name)).map(r=>r.name)');const gifHashes=await Promise.all(gifSrc.map(async s=>hash(await getBytes(s))));assert.ok(gifHashes.includes(hash(fs.readFileSync(path.join(out,'qa/original/gif3.gif')))));passed.push('Large GIF bytes preserved');
  await scene(0,0);await sleep(200);const cache=await evaluate(url,"fetch('/probe-stats/current').then(r=>r.json())");assert.equal(cache.encodedBytes,0);passed.push('Unmount releases all ImageAssets binary buffers');
  const result={environment:env,passed,manualTests:false,scope:'Native production image component and asset transport; excludes accounts, network and DM state',exeSha256:hash(fs.readFileSync(exe))};fs.writeFileSync(path.join(root,'target/image-reports/UNIFIED_IMAGE_CHECKS.json'),JSON.stringify(result,null,2)+'\n');console.log(result);
 }finally{
  channel?.ws.close();const clean=spawnSync('powershell.exe',['-NoProfile','-Command',"$p=Get-Process -Id "+child.pid+" -ErrorAction SilentlyContinue;if($p -and $p.Path -eq '"+exe.replaceAll("'","''")+"'){[void]$p.CloseMainWindow();if(-not $p.WaitForExit(5000)){taskkill /PID "+child.pid+" /T /F}};'closed'"],{encoding:'utf8',windowsHide:true});fs.writeFileSync(path.join(folder,'cleanup.log'),clean.stdout+clean.stderr);await sleep(1500);if(child.exitCode===null)throw Error('Owned probe remains');
 }
})().catch(e=>{console.error(e);process.exitCode=1});

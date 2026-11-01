const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const root = path.resolve(__dirname, '..');
fs.mkdirSync(path.join(root, 'target/image-reports'), { recursive: true });
const out = path.join(root, 'target/image-memory');
const experimental = process.argv.includes('--thumbnail-experiment');
const exeIndex = process.argv.indexOf('--exe');
const exe = exeIndex >= 0 ? path.resolve(process.argv[exeIndex + 1]) : experimental ? path.join(out, 'DiscordiaImagePaintThumbnails.exe') : path.join(root, 'target/release/DiscordiaImageProbe.exe');
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
  const child=spawn(exe,[],{cwd:root,env:{...process.env,IMAGE_PROBE_DATA:path.join(out,'fixtures'),WEBVIEW2_USER_DATA_FOLDER:path.join(folder,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-address=127.0.0.1 --remote-debugging-port=19349'},stdio:'ignore'});
  child.on('error',console.error);
  const passed=[];
  try {
    let url;
    for(let i=0;i<200;i++){
      try {const tabs=await(await fetch('http://127.0.0.1:19349/json/list')).json(); const p=tabs.find(p=>p.type==='page'&&p.url.includes('dioxus')); if(p&&await evaluate(p.webSocketDebuggerUrl,'!!document.querySelector("#probe")')){url=p.webSocketDebuggerUrl;break;}}catch{}
      await sleep(100);
    }
    assert.ok(url,'probe ready');
    // Emulation overrides reset when their CDP session detaches.
    await connect(url);
    const scene=async n=>{
      await evaluate(url,`document.querySelector('#scenario-${n}').click()`);
      for(let i=0;i<200;i++){
        if(await evaluate(url,`document.querySelector('#probe').dataset.scenario==='${n}'`))break;
        await sleep(25);
      }
      return evaluate(url,`Promise.all([...document.querySelectorAll('#images img')].map(async i=>{for(let n=0;n<5;n++){const src=i.src;try{await i.decode();break}catch(error){if(i.src===src)throw error;await new Promise(r=>setTimeout(r,40))}}return {src:i.getAttribute('src'),current:i.currentSrc,set:i.parentElement.querySelector('source')?.getAttribute('srcset'),w:i.naturalWidth,h:i.naturalHeight}}))`);
    };
    const env=await evaluate(url,'({dpr:devicePixelRatio,visibility:document.visibilityState,focus:document.hasFocus()})');
    const thumbs=await scene(2);
    assert.equal(thumbs.length,8);assert.equal(thumbs[0].w,experimental?640:4000);assert.equal(thumbs[0].h,experimental?480:3000);
    if(experimental)assert.ok(thumbs[0].current.endsWith(thumbs[0].set));
    passed.push(experimental?'JPEG inline uses a 640x480 derivative at native probe density':'JPEG inline preserves the original image source');
    const original=thumbs[0].src;
    const received=await evaluate(url,`(async()=>{const r=await fetch(${JSON.stringify(original)}),a=new Uint8Array(await r.arrayBuffer());let s='';for(let i=0;i<a.length;i+=32768)s+=String.fromCharCode(...a.subarray(i,i+32768));return btoa(s)})()`);
    assert.equal(hash(Buffer.from(received,'base64')),hash(fs.readFileSync(path.join(out,'fixtures/high0.jpg'))));
    passed.push('original asset bytes match the source SHA-256');
    await rpc(url,'Emulation.setDeviceMetricsOverride',{width:1440,height:900,deviceScaleFactor:3,mobile:false});
    await scene(0);await scene(2);
    for(let i=0;i<100;i++){
      if(await evaluate(url,`document.querySelector('#images img').currentSrc.endsWith(${JSON.stringify(original)})`))break;
      await sleep(100);
    }
    const dense=await evaluate(url,'Promise.all([...document.querySelectorAll("#images img")].map(async i=>{await i.decode();return {src:i.currentSrc,w:i.naturalWidth,h:i.naturalHeight}}))');
    assert.equal(await evaluate(url,'devicePixelRatio'),3);assert.ok(dense[0].src.endsWith(original));
    passed.push('emulated 3x mount selects the untouched original');
    await rpc(url,'Emulation.clearDeviceMetricsOverride');
    const full=await scene(7);assert.equal(full.length,1);assert.equal(full[0].w,4000);assert.ok(full[0].current.endsWith(original));
    for(let i=1;i<=6;i++){await evaluate(url,"document.querySelector('#next').click()");await sleep(40);}
    await sleep(100);
    assert.equal(await evaluate(url,'document.querySelectorAll("#images img").length'),1);
    passed.push('six full-view open/close transitions preserve the original');
    const avatars=await scene(3);assert.equal(avatars.length,36);assert.equal(avatars[0].w,1024);assert.ok(avatars.every(i=>!i.set));
    const avatarUrl=avatars[0].src;assert.notEqual(avatars[0].src,avatars[1].src);
    await scene(0);const reopened=await scene(3);assert.equal(reopened[0].src,avatarUrl);
    passed.push('distinct avatar payloads have distinct URLs; remount keeps the same URL');
    await scene(4);
    const gifUrls=await evaluate(url,'performance.getEntriesByType("resource").filter(e=>/\\.gif$/.test(e.name)).map(e=>e.name)');
    assert.ok(gifUrls.length>=4);
    const gifBytes=await evaluate(url,`(async()=>{const a=new Uint8Array(await(await fetch(${JSON.stringify(gifUrls[0])})).arrayBuffer());let s='';for(let i=0;i<a.length;i+=32768)s+=String.fromCharCode(...a.subarray(i,i+32768));return btoa(s)})()`);
    assert.ok([0,1,2,3].map(i=>hash(fs.readFileSync(path.join(out,'fixtures',`gif${i}.gif`)))).includes(hash(Buffer.from(gifBytes,'base64'))));
    await evaluate(url,"window.dispatchEvent(new Event('focus'))");await sleep(200);
    const frame=async()=>{
      const clip=await evaluate(url,"(()=>{const r=document.querySelector('#images img').getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,scale:1}})()");
      return (await rpc(url,'Page.captureScreenshot',{format:'png',clip})).data;
    };
    const before=await frame();await sleep(240);const after=await frame();assert.notEqual(before,after);
    passed.push('GIF bytes are unchanged and animation advances after focus event');
    await scene(0);await sleep(200);
    const cache=await evaluate(url,"fetch('/probe-stats/current').then(r=>r.json())");assert.equal(cache.encodedBytes,0);
    passed.push('unmount releases all binary buffers');
    const result={passed,environment:env,exeSha256:hash(fs.readFileSync(exe)),thumbnailExperiment:experimental,manualTests:false};
    fs.writeFileSync(path.join(root,experimental?'target/image-reports/RAM_PHASE8_THUMBNAIL_CHECKS.json':'target/image-reports/RAM_PHASE8_CHECKS.json'),JSON.stringify(result,null,2)+'\n');
    console.log(result);
  } finally {
    channel?.ws.close();
    const closed=spawnSync('powershell.exe',['-NoProfile','-Command',`$p=Get-Process -Id ${child.pid} -ErrorAction SilentlyContinue;if($p -and $p.Path -eq '${exe.replaceAll("'","''")}'){[void]$p.CloseMainWindow();if(-not $p.WaitForExit(5000)){taskkill /PID ${child.pid} /T /F}};'closed'`],{encoding:'utf8'});
    fs.writeFileSync(path.join(folder,'cleanup.log'),closed.stdout+closed.stderr);await sleep(2000);
    if(child.exitCode===null)throw new Error('Owned check process remains');
  }
})().catch(error=>{console.error(error);process.exitCode=1});

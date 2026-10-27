const {chromium}=require('playwright');
const fs=require('node:fs');
const assert=require('node:assert/strict');
const script=fs.readFileSync('client/assets/image-viewer.js','utf8');
const css=fs.readFileSync('client/assets/image-viewer.css','utf8');
const picture='data:image/svg+xml;base64,'+Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600"><rect width="800" height="600" fill="#ba7b38"/></svg>').toString('base64');
async function fixture(page,token='first'){
 await page.setContent(`<meta name="viewport" content="width=device-width,initial-scale=1"><style>*{box-sizing:border-box}body{margin:0}:root{--border:#333;--panel-solid:#181818;--text:#eee;--text-muted:#aaa;--accent:#e8933a}button{background:none;border:0;color:inherit}${css}</style><button id="previous">Previous</button><div id="dxf-image-viewer" class="dxf-image-viewer" tabindex="-1"><div class="dxf-image-stage" data-viewer-stage><img src="${picture}" draggable="false"></div><div class="dxf-image-controls"><button data-viewer-action="out">−</button><button data-viewer-action="reset"><span data-viewer-zoom></span></button><button data-viewer-action="in">+</button><button data-viewer-action="close">×</button></div></div>`);
 await page.locator('img').evaluate(i=>i.decode());
 await page.evaluate(()=>{document.getElementById('previous').focus();window.closedEvents=[];window.dioxus={send:v=>closedEvents.push(v)};});
 await page.evaluate(([source,token])=>(0,eval)(`(${source})`)(token),[script,token]);
}
const zoom=page=>page.locator('#dxf-image-viewer').getAttribute('data-zoom').then(Number);
const settle=page=>page.evaluate(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))));
(async()=>{
 const browser=await chromium.launch({headless:true,executablePath:process.env.BROWSER_PATH||undefined});
 try{
  const page=await browser.newPage({viewport:{width:1280,height:800}});
  await fixture(page);assert.equal(await zoom(page),1);
  assert.equal(await page.locator('[data-viewer-action=out]').isDisabled(),true);
  await page.mouse.move(640,430);await page.mouse.wheel(0,-200);await settle(page);
  assert((await zoom(page))>1.5);
  await page.mouse.move(640,430);await page.mouse.down();await page.mouse.move(740,470,{steps:5});await page.mouse.up();await settle(page);
  assert.equal(await page.evaluate(()=>closedEvents.length),0);
  assert(await page.locator('img').evaluate(i=>new DOMMatrixReadOnly(getComputedStyle(i).transform).m41)>0);
  await page.locator('[data-viewer-action=reset]').click();await settle(page);assert.equal(await zoom(page),1);
  await page.locator('img').dblclick();await settle(page);assert.equal(await zoom(page),2);
  await page.keyboard.press('0');await settle(page);assert.equal(await zoom(page),1);
  await page.keyboard.press('+');await settle(page);assert((await zoom(page))>1);
  await page.keyboard.press('Tab');assert.equal(await page.evaluate(()=>document.activeElement.dataset.viewerAction),'out');
  await page.keyboard.press('Escape');assert.deepEqual(await page.evaluate(()=>closedEvents),['close']);
  await page.evaluate(()=>window.__dxfImageViewerOff());assert.equal(await page.evaluate(()=>document.activeElement.id),'previous');
  console.log('Desktop: wheel, pan, double click, reset, keyboard, close and focus restoration passed');
  const mobile=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  const phone=await mobile.newPage();await fixture(phone);
  const cdp=await mobile.newCDPSession(phone);
  const touch=(type,touchPoints)=>cdp.send('Input.dispatchTouchEvent',{type,touchPoints});
  await touch('touchStart',[{x:150,y:420,id:1},{x:240,y:420,id:2}]);
  await touch('touchMove',[{x:100,y:420,id:1},{x:290,y:420,id:2}]);await settle(phone);
  assert((await zoom(phone))>1.8);
  await touch('touchEnd',[]);await touch('touchStart',[{x:100,y:420,id:3}]);await touch('touchMove',[{x:65,y:450,id:3}]);await touch('touchEnd',[]);await settle(phone);
  assert.equal(await phone.evaluate(()=>closedEvents.length),0);
  await phone.locator('[data-viewer-action=reset]').tap();await phone.waitForFunction(()=>Number(document.getElementById('dxf-image-viewer').dataset.zoom)===1,null,{timeout:3000});assert.equal(await zoom(phone),1);
  const controls=await phone.locator('.dxf-image-controls').boundingBox();assert(controls.x>=0&&controls.x+controls.width<=390);
  for(const box of await phone.locator('.dxf-image-controls button').evaluateAll(nodes=>nodes.map(n=>({w:n.clientWidth,h:n.clientHeight}))))assert(box.w>=44&&box.h>=44);
  await phone.setViewportSize({width:844,height:390});await settle(phone);assert.equal(await zoom(phone),1);
  await touch('touchStart',[{x:5,y:200,id:5}]);await touch('touchEnd',[]);assert.deepEqual(await phone.evaluate(()=>closedEvents),['close']);
  await phone.evaluate(()=>window.__dxfImageViewerOff());await mobile.close();
  console.log('Mobile: real touch pinch, one-finger pan, controls, rotation and backdrop close passed');
  await fixture(page,'old');await page.evaluate(()=>window.oldOff=window.__dxfImageViewerOff);
  await fixture(page,'new');await page.evaluate(()=>window.oldOff());
  await page.locator('[data-viewer-action=in]').click();await settle(page);assert((await zoom(page))>1);
  await page.evaluate(()=>window.__dxfImageViewerOff());
  console.log('Cleanup: a stale viewer cannot remove replacement handlers');
 }finally{await browser.close();console.log('Temporary browser closed:',!browser.isConnected());}
})().catch(e=>{console.error(e);process.exitCode=1;});

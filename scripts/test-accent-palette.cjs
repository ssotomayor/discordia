const {chromium}=require('playwright');
const fs=require('node:fs');
const assert=require('node:assert/strict');
const vm=require('node:vm');
const source=fs.readFileSync('client/src/features/accent_palette.rs','utf8');
const tabScript=source.match(/document::eval\(&format!\("([^"\n]*)"\)\)/)[1];
for(const step of [-1,1])for(const index of [-1,0,2]){
 let focused=-1;
 const nodes=Array.from({length:3},(_,i)=>({focus:()=>focused=i}));
 const document={activeElement:nodes[index],getElementById:()=>({querySelectorAll:()=>nodes})};
 vm.runInNewContext(tabScript.replaceAll('{step}',String(step)),{document});
 assert.equal(focused,index<0?(step<0?2:0):(index+step+3)%3);
}
const styles=fs.readFileSync('client/assets/tailwind.css','utf8')+fs.readFileSync('client/assets/accent-palette.css','utf8');
const html=`<style>:root{--panel-solid:#17150f;--bg:#121110;--border:#3a342b;--text:#f2ece3;--text-muted:#9a9287;--accent:#e8933a}body{background:#121110;font-family:Arial}${styles}</style><body><div class="dxf-accent-backdrop"><section class="dxf-accent-palette"><h3>Color de la comunidad</h3><div class="dxf-accent-field" style="background-color:hsl(27.5 100% 50%)"><span class="dxf-accent-cursor" style="left:53%;top:12%"></span></div><label class="dxf-accent-hue-label"><span>Tono</span><input type="range" class="dxf-accent-hue" min="0" max="359" value="27.5"></label><div class="dxf-accent-rgb">${['R','G','B'].map((x,i)=>`<label>${x}<input type="number" value="${[224,160,106][i]}"></label>`).join('')}</div><label class="dxf-accent-hex">HEX<input value="#e0a06a"><span class="dxf-accent-swatch" style="background-color:#e0a06a"></span></label><button class="dxf-accent-reset">Predeterminado del tema</button><footer><button>Cancelar</button><button class="dxf-accent-apply">Aplicar</button></footer></section></div></body>`;
(async()=>{const browser=await chromium.launch({executablePath:process.env.BROWSER_PATH||undefined,headless:true});try{
 const page=await browser.newPage();
 for(const [width,height,mobile] of [[320,640,true],[390,844,true],[800,360,true],[1280,800,false]]){
  await page.setViewportSize({width,height});await page.setContent(html);if(mobile)await page.locator('body').evaluate(x=>x.classList.add('dxf-platform-mobile'));
  await page.locator('section').evaluate(x=>x.scrollTop=x.scrollHeight);
  const geometry=await page.evaluate(()=>{const p=document.querySelector('section').getBoundingClientRect();const f=document.querySelector('footer').getBoundingClientRect();return {p:{left:p.left,right:p.right,top:p.top,bottom:p.bottom},f:{left:f.left,right:f.right,top:f.top,bottom:f.bottom},overflow:document.documentElement.scrollWidth>innerWidth}});
  assert(!geometry.overflow);assert(geometry.f.left>=geometry.p.left&&geometry.f.right<=geometry.p.right);assert(geometry.f.bottom<=height);assert(geometry.f.top>=0);
  if(width===390&&process.env.SCREENSHOT_PATH)await page.screenshot({path:process.env.SCREENSHOT_PATH});
  console.log(JSON.stringify({width,height,buttonsInsidePalette:true,footerReachable:true}));
 }
}finally{await browser.close();console.log('Temporary browser closed:',!browser.isConnected());}})().catch(e=>{console.error(e);process.exitCode=1;});

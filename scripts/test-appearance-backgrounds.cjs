const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const app = fs.readFileSync('client/src/app.rs', 'utf8');
const appearance = fs.readFileSync('client/src/features/appearance.rs', 'utf8');
const css = app.match(/const BASE_CSS: &str = "([\s\S]*?)";/)[1].replace(/\\"/g, '"');
const themes = [...app.matchAll(/ThemeDef\s*\{\s*id:\s*"([^"]+)",\s*label:\s*"([^"]+)",\s*swatch:\s*"([^"]+)"[\s\S]*?vars:\s*"([^"]+)"/g)]
  .map(([,id,label,swatch,vars])=>({id,label,swatch,vars:vars.replace(/\\\r?\n\s*/g,'')}));
const previews = new Map([...appearance.matchAll(/\(\s*"([^"]+)",\s*"[^"]+",\s*"([^"]+)"/g)].map(([,id,style])=>[id,style]));
assert.equal(themes.length,10);
assert.equal(new Set(themes.map(theme=>theme.id)).size,10);
assert.ok(previews.has('mesh') && previews.has('none'));

(async()=>{
  const browser = await chromium.launch({executablePath:process.env.BROWSER_PATH||undefined,headless:true});
  try {
    const page = await browser.newPage();
    let checked = 0;
    for(const theme of themes) for(const [width,height] of [[320,640],[800,600]]) {
      await page.setViewportSize({width,height});
      await page.setContent(`<style>:root{${theme.vars}}body{margin:0;background-color:var(--bg)}${css}</style><div id="background" class="app-bg-pattern"></div>`);
      const plain = await page.screenshot();
      await page.locator('#background').evaluate(node=>node.classList.add('app-bg-mesh'));
      const painted = await page.locator('#background').evaluate(node=>({image:getComputedStyle(node).backgroundImage,color:getComputedStyle(node).backgroundColor}));
      assert.equal((painted.image.match(/radial-gradient/g)||[]).length,2,`${theme.id}: mesh layers rejected`);
      assert.notEqual(painted.color,'rgba(0, 0, 0, 0)',`${theme.id}: missing base`);
      assert.notDeepEqual(await page.screenshot(),plain,`${theme.id}: mesh looks identical to none`);
      await page.setContent(`<style>:root{${theme.vars}}body{margin:0}</style><div id="preview" style="width:110px;height:56px;${previews.get('none')}"></div>`);
      const tile = page.locator('#preview');
      const none = await tile.screenshot();
      await tile.evaluate((node,style)=>{node.style.cssText=`width:110px;height:56px;${style}`;},previews.get('mesh'));
      assert.match(await tile.evaluate(node=>getComputedStyle(node).backgroundImage),/radial-gradient/,`${theme.id}: preview rejected`);
      assert.notDeepEqual(await tile.screenshot(),none,`${theme.id}: preview looks identical to none`);
      checked++;
    }
    console.log(`Mesh background and preview: ${checked} theme/viewport combinations passed`);
    const popupClass=appearance.match(/class: "(dxf-pop-in fixed[^"\n]+)"/)[1];
    const popupStyle=appearance.match(/style: "(width: min\(320px[^"\n]+)"/)[1];
    const tailwind=fs.readFileSync('client/assets/tailwind.css','utf8');
    for(const [width,height] of [[320,640],[390,844],[800,360],[1280,800]]) {
      await page.setViewportSize({width,height});
      const position=popupStyle.replaceAll('{at_x}',String(width-24)).replaceAll('{at_y}','44');
      await page.setContent(`<style>:root{${themes[0].vars}}${tailwind}${css}</style><section id="appearance" class="${popupClass}" style="${position}"><div class="dxf-theme-grid">${themes.map(theme=>`<button type="button" class="dxf-theme-tile" aria-label="${theme.label}"><span class="dxf-theme-swatch" style="${theme.vars}background-color:var(--bg);background-image:linear-gradient(150deg,${theme.swatch},transparent 78%)"></span><span class="dxf-theme-name">${theme.label}</span></button>`).join('')}</div></section>`);
      await page.locator('#appearance').evaluate(async node=>{await Promise.all(node.getAnimations({subtree:true}).map(animation=>animation.finished.catch(()=>{})));});
      const layout=await page.evaluate(()=>{
        const box=node=>{const r=node.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:r.width,height:r.height};};
        const tiles=[...document.querySelectorAll('.dxf-theme-tile')];
        return {popup:box(document.getElementById('appearance')),tiles:tiles.map(box),swatches:[...document.querySelectorAll('.dxf-theme-swatch')].map(box),clipped:[...document.querySelectorAll('.dxf-theme-name')].some(node=>node.scrollWidth>node.clientWidth+1)};
      });
      assert.ok(layout.popup.left>=0&&layout.popup.right<=width,`popover clips at ${width}`);
      assert.ok(layout.popup.top>=0&&layout.popup.bottom<=height,`popover exceeds height at ${width}`);
      assert.equal(layout.clipped,false,`theme name clips at ${width}`);
      const rows=new Map();
      for(const tile of layout.tiles) {const y=Math.round(tile.top);rows.set(y,(rows.get(y)||0)+1);assert.ok(tile.width>=44,`touch target at ${width}`);}
      assert.deepEqual([...rows.values()],[5,5],`not two rows of five at ${width}`);
      for(const swatch of layout.swatches) assert.equal(swatch.height,44);
    }
    console.log('Theme selector: two rows of five, readable names and touch targets passed at 320/390/800/1280px');
  } finally {
    await browser.close();
    console.log('Temporary browser closed:',!browser.isConnected());
  }
})().catch(error=>{console.error(error);process.exitCode=1;});

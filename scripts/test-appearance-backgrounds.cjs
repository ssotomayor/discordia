const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const app = fs.readFileSync('client/src/app.rs', 'utf8');
const appearance = fs.readFileSync('client/src/features/appearance.rs', 'utf8');
const css = app.match(/const BASE_CSS: &str = "([\s\S]*?)";/)[1].replace(/\\"/g, '"');
const themes = [...app.matchAll(/ThemeDef\s*\{\s*id:\s*"([^"]+)"[\s\S]*?vars:\s*"([^"]+)"/g)]
  .map(([,id,vars])=>({id,vars:vars.replace(/\\\r?\n\s*/g,'')}));
const previews = new Map([...appearance.matchAll(/\(\s*"([^"]+)",\s*"[^"]+",\s*"([^"]+)"/g)].map(([,id,style])=>[id,style]));
assert.equal(themes.length,5);
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
  } finally {
    await browser.close();
    console.log('Temporary browser closed:',!browser.isConnected());
  }
})().catch(error=>{console.error(error);process.exitCode=1;});

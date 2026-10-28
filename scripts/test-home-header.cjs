const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const home = fs.readFileSync('client/src/features/home.rs', 'utf8');
const app = fs.readFileSync('client/src/app.rs', 'utf8');
const icons = fs.readFileSync('client/src/features/icons.rs', 'utf8');
const css = fs.readFileSync('client/assets/tailwind.css', 'utf8') + '\n' + app.match(/const BASE_CSS: &str = "([\s\S]*?)";/)[1].replace(/\\"/g, '"') + '\n' + fs.readFileSync('client/assets/mobile.css', 'utf8');
const cls = prefix => home.match(new RegExp('class: "(' + prefix + '[^"]*)"'))[1].replace('{social_cls}', '').replace('{mac_header}', '');
const icon = name => icons.match(new RegExp('pub const ' + name + ': &str = r##"([\\s\\S]*?)"##;'))[1];
const logo = app.match(/const DISCORDIA_LOGO_SVG: &str = r##"([\s\S]*?)"##;/)[1].replace('viewBox="0 0 1024 1024"', 'viewBox="200 190 650 630"');
const button = (name, svg) => `<button class="${cls(name)}"><span class="block w-4 h-4">${svg}</span></button>`;
const html = (mobile) => `<style>${css}</style><body class="${mobile ? "dxf-platform-mobile" : ""}" style="background:var(--bg);color:var(--text)">
<div class="${cls('dxf-home-bar')}"><div class="${cls('dxf-home-header')}">
<div class="${cls('dxf-home-brand')}"><div class="dxf-logo ${cls('dxf-home-logo')}">${logo}</div><span class="dxf-wordmark">Discordia</span></div>
<span class="${cls('dxf-home-relays')}"><span style="width:7px;height:7px;background:var(--up);border-radius:50%"></span><span class="text-xs">4/4</span></span>
<div class="${cls('dxf-home-spacer')}"></div><div class="${cls('dxf-home-tools')}">
<button class="${cls('dxf-home-social')}" aria-expanded="true" aria-label="Social 3"><span>${icon('USERS')}</span><span class="dxf-home-social-label">Social</span><span>3</span></button>
${mobile ? `${button('dxf-home-settings', icon('GEAR'))}<button class="${cls('dxf-home-account')}"><div class="dxf-avatar w-8 h-8 border rounded-md flex items-center justify-center"><img alt="Account" style="width:100%;height:100%;object-fit:cover" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='32' height='32'%3E%3Crect width='32' height='32' fill='red'/%3E%3C/svg%3E"></div><div class="${cls('dxf-home-account-details')}">Nombre de usuario</div></button>` : `<button class="${cls('dxf-home-account')}"><div class="dxf-avatar w-8 h-8 border rounded-md flex items-center justify-center"><img alt="Account" style="width:100%;height:100%;object-fit:cover" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='32' height='32'%3E%3Crect width='32' height='32' fill='red'/%3E%3C/svg%3E"></div><div class="${cls('dxf-home-account-details')}">Nombre de usuario</div></button>${button('dxf-home-settings', icon('GEAR'))}`}
</div></div></div><p style="padding:16px">Inicio</p>
<div class="dxf-workspace-mobile p-2" style="padding-top:max(var(--safe-top),.5rem);padding-left:max(var(--safe-left),.5rem);padding-right:max(var(--safe-right),.5rem)">
<div class="dxf-workspace-header flex items-center gap-2"><div class="dxf-workspace-brand flex items-center"><div class="dxf-logo dxf-workspace-logo w-6 h-6">${logo}</div></div>
<div class="dxf-connection-controls flex items-center border"><span style="padding:3px 8px;font-size:11px">Servidor · Direct IPv4</span></div>
<div class="dxf-workspace-tools ml-auto flex items-center gap-1.5">${['PALETTE','GEAR','GEAR'].map((name, i) => `<button class="w-8 h-8 flex items-center justify-center border rounded-lg ${i===1?'server-settings':''}"><span class="block w-4 h-4">${icon(name)}</span></button>`).join('')}</div></div></div><p style="padding:16px">Servidor</p>`;
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.BROWSER_PATH || undefined, headless: true });
  try {
    const page = await browser.newPage();
    for (const width of [320, 360, 390, 412, 768, 1280]) {
      await page.setViewportSize({width, height: 700});
      await page.setContent(html(true));
      const result = await page.evaluate(() => {
        const box = sel => { const r = document.querySelector(sel).getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height,cx:r.x+r.width/2,cy:r.y+r.height/2}; };
        return { home:box('.dxf-home-settings'), server:box('.server-settings'), homeRoot:box('.dxf-home-bar'), serverRoot:box('.dxf-workspace-mobile'), homeLogo:box('.dxf-home-logo'), serverLogo:box('.dxf-workspace-logo'), icon:box('.dxf-home-settings svg'), social:box('.dxf-home-social'), relays:box('.dxf-home-relays'), overflow:document.documentElement.scrollWidth > innerWidth };
      });
      assert.equal(result.overflow, false, `overflow ${width}`);
      assert.equal(result.home.w,44); assert.equal(result.home.h,44);
      assert.equal(result.home.cx,result.server.cx, `settings x ${width}`);
      assert.equal(result.home.cy-result.homeRoot.y,result.server.cy-result.serverRoot.y, `settings y ${width}`);
      assert.equal(result.homeLogo.w,result.serverLogo.w);
      assert.equal(result.homeLogo.h,result.serverLogo.h);
      assert.equal(result.home.cx,result.icon.cx); assert.equal(result.home.cy,result.icon.cy);
      assert.ok(result.relays.x+result.relays.w <= result.social.x, `overlap ${width}`);
      assert.equal(await page.getByRole('button', {name: 'Social 3', exact:true}).count(), 1);
      const avatar = await page.locator('.dxf-home-account').boundingBox();
      const exit = await page.locator('.dxf-workspace-tools > button').last().boundingBox();
      assert.equal(avatar.width, 44); assert.equal(avatar.height, 44);
      assert.equal(avatar.width, exit.width); assert.equal(avatar.height, exit.height);
      assert.equal(avatar.x + avatar.width / 2, exit.x + exit.width / 2);
      const picture = await page.locator('.dxf-home-account img').boundingBox();
      assert.equal(picture.width, 42); assert.equal(picture.height, 42);
      const order = await page.locator('.dxf-home-tools > button').evaluateAll(nodes => nodes.map(n => n.className.split(' ')[0]));
      assert.deepEqual(order, ['dxf-home-social','dxf-home-settings','dxf-home-account']);
      console.log(JSON.stringify({width,settingsX:result.home.cx,settingsY:result.home.cy,logo:result.homeLogo.w,icon:result.icon.w,overflow:result.overflow}));
    }
    await page.setViewportSize({width:1280,height:700});
    await page.setContent(html(false));
    await page.keyboard.press('Tab');
    assert.equal(await page.locator(':focus').getAttribute('aria-label'), 'Social 3');
    await page.keyboard.press('Tab');
    assert.ok((await page.locator(':focus').getAttribute('class')).startsWith('dxf-home-account'));
    await page.keyboard.press('Tab');
    assert.ok((await page.locator(':focus').getAttribute('class')).startsWith('dxf-home-settings'));
    const desktop = await page.locator('.dxf-home-settings').boundingBox();
    await page.locator('.dxf-home-header').evaluate(node => node.classList.add('dxf-home-macos'));
    const mac = await page.locator('.dxf-home-settings').boundingBox();
    assert.equal(mac.y-desktop.y,8);
    await page.locator('body').evaluate(node => node.classList.add('dxf-platform-mobile'));
    assert.equal(await page.locator('.dxf-home-settings').evaluate(node => getComputedStyle(node).position), 'static');
  } finally { await browser.close(); console.log('Temporary browser closed:', !browser.isConnected()); }
})().catch(error => { console.error(error); process.exitCode=1; });

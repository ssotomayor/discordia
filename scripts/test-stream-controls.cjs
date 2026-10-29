const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const script = fs.readFileSync('client/assets/stream-controls.js', 'utf8');
const css = ['tailwind.css', 'mobile.css', 'stream-viewer.css'].map(file => fs.readFileSync(`client/assets/${file}`, 'utf8')).join('\n');
const fixture = (popout = false, mobile = false, expanded = false) => `<style>${css}
:root{--text:#eee;--text-muted:#bbb;--text-dim:#999;--border:#444;--panel-solid:#16191e;--panel2:#252b32;--accent:#e4b658;--danger:#ef4444}body{margin:0;font:14px Arial;background:#222}#tile{height:360px;width:100%}.dxf-stream-video{background:linear-gradient(130deg,#194854,#5f7762)}button{color:inherit}</style>
<button id="outside">Outside</button><div class="${mobile ? 'dxf-platform-mobile' : ''}"><div class="dxf-streams ${expanded ? 'dxf-streams-expanded' : ''}"><div id="tile" class="dxf-stream-tile flex flex-col min-w-0 min-h-0 border rounded-lg overflow-hidden" data-stream-controls>
<div class="dxf-stream-toolbar flex flex-wrap items-center gap-2 shrink-0"><span class="dxf-stream-name min-w-0 flex-1 truncate">Screen of a long name</span><span tabindex="0" id="viewers">1 viewer</span><button class="dxf-stream-icon-button" id="focus">▦</button><button class="dxf-stream-icon-button" id="full">⛶</button><button class="dxf-stream-icon-button" id="stop">×</button></div>
<div class="dxf-stream-video flex-1 min-h-0" id="video" role="button" tabindex="0"></div>
<div class="dxf-stream-footer ${popout ? 'dxf-stream-footer-popout grid' : 'flex flex-wrap justify-between'} items-center gap-2 shrink-0"><div class="min-w-0"><span>1920×1080 · 30 FPS</span></div><div class="flex items-center gap-2"><button class="dxf-stream-icon-button" id="mute">◖</button><input id="volume" type="range" min="0" max="100" value="50" class="dxf-stream-volume w-24"><span>50%</span></div>${popout ? '<div class="min-w-0 justify-self-end"><div class="flex items-center gap-2"><button class="dxf-stream-icon-button">P</button><button class="dxf-stream-icon-button">F</button><button class="dxf-stream-icon-button">D</button></div></div>' : ''}</div></div></div></div>`;

(async () => {
  const browser = await chromium.launch({ executablePath: process.env.BROWSER_PATH || undefined, headless: true });
  try {
    const page = await browser.newPage();
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await page.clock.install();
    const mount = async (popout = false, mobile = false, expanded = false) => {
      await page.evaluate(() => window.dxStreamControls?.destroy());
      await page.setContent(fixture(popout, mobile, expanded));
      await page.evaluate(() => {
        window.videoClicks = 0;
        document.getElementById('video').onclick = () => window.videoClicks++;
      });
      await page.addScriptTag({ content: script });
    };
    const visible = () => page.locator('#tile').getAttribute('data-controls-visible');
    const pointer = (type, extra = {}, target = '#tile') => page.locator(target).evaluate((node, args) => node.dispatchEvent(new PointerEvent(args.type, { bubbles: true, pointerType: 'mouse', ...args.extra })), { type, extra });
    await mount();
    await page.clock.fastForward(2600);
    assert.equal(await visible(), 'false');
    await pointer('pointermove'); assert.equal(await visible(), 'true');
    await page.clock.fastForward(2000); assert.equal(await visible(), 'true');
    await pointer('pointermove'); await page.clock.fastForward(1000); assert.equal(await visible(), 'true');
    await pointer('pointerleave'); assert.equal(await visible(), 'false');
    await pointer('pointerenter'); assert.equal(await visible(), 'true');
    await pointer('pointerdown', {}, '#volume');
    await pointer('pointerleave'); await page.clock.fastForward(5000); assert.equal(await visible(), 'true');
    await page.evaluate(() => window.dispatchEvent(new PointerEvent('pointerup', { clientX: -1, clientY: -1 })));
    assert.equal(await visible(), 'false');

    await page.locator('#outside').focus();
    await page.keyboard.press('Tab');
    await page.clock.fastForward(5000); assert.equal(await visible(), 'true', 'keyboard focus keeps controls available');
    await pointer('pointerleave'); assert.equal(await visible(), 'true');
    await page.locator('#outside').focus(); assert.equal(await visible(), 'false');
    await pointer('pointermove');
    await page.locator('#full').click();
    await page.clock.fastForward(2600); assert.equal(await visible(), 'false', 'a mouse-click focus must not keep controls stuck on');
    await pointer('pointerenter'); await pointer('pointerdown', {}, '#volume');
    await page.evaluate(() => window.dispatchEvent(new Event('blur')));
    assert.equal(await visible(), 'false', 'losing window focus releases a drag');

    await page.addScriptTag({ content: script });
    await page.locator('#tile').evaluate(node => { window.removedTile = node; node.remove(); });
    await page.clock.fastForward(5000);
    assert.equal(await page.evaluate(() => window.removedTile.hasAttribute('data-controls-visible')), false);
    await page.evaluate(() => document.body.insertAdjacentHTML('beforeend', '<div id="tile" class="dxf-stream-tile" data-stream-controls><div class="dxf-stream-toolbar"><button>New tile</button></div></div>'));
    assert.equal(await visible(), 'true');
    await page.evaluate(() => window.dxStreamControls.destroy());
    assert.equal(await visible(), null);
    await pointer('pointermove'); assert.equal(await visible(), null);

    for (const popout of [false, true]) for (const mobile of [false, true]) for (const expanded of [false, true]) {
      for (const width of [320, 640, 1280]) {
        await page.setViewportSize({ width, height: 800 });
        await mount(popout, mobile, expanded);
        const layout = await page.evaluate(() => {
          const box = node => { const r = node.getBoundingClientRect(); return { left: r.left, top: r.top, right: r.right, bottom: r.bottom, width: r.width, height: r.height }; };
          return { tile: box(document.getElementById('tile')), video: box(document.getElementById('video')), bars: [...document.querySelectorAll('.dxf-stream-toolbar,.dxf-stream-footer')].map(box), actions: [...document.querySelectorAll('#tile button,#tile input')].map(box) };
        });
        const context = `${popout}/${mobile}/${expanded}/${width}`;
        assert.ok(layout.video.height > 0, context);
        assert.ok(Math.abs(layout.video.height - layout.tile.height) <= 2, context);
        assert.ok(Math.abs(layout.video.width - layout.tile.width) <= 2, context);
        for (const bar of layout.bars) assert.ok(bar.top >= layout.tile.top && bar.bottom <= layout.tile.bottom + 1, context);
        for (const action of layout.actions) assert.ok(action.left >= layout.tile.left && action.right <= layout.tile.right + 1, context);
      }
    }
    console.log('Stream controls: overlays fill the video, idle/leave hiding, keyboard, dragging, focus loss, cleanup and responsive layouts passed');
    const touch = await browser.newContext({ hasTouch: true, viewport: { width: 640, height: 480 } });
    try {
      const page = await touch.newPage();
      await page.setContent(fixture());
      await page.clock.install();
      await page.evaluate(() => { window.videoClicks = 0; document.getElementById('video').onclick = () => window.videoClicks++; });
      await page.addScriptTag({ content: script });
      await page.clock.fastForward(2600);
      const rect = await page.locator('#video').boundingBox();
      await page.touchscreen.tap(rect.x + rect.width / 2, rect.y + rect.height / 2);
      assert.equal(await page.locator('#tile').getAttribute('data-controls-visible'), 'true');
      assert.equal(await page.evaluate(() => window.videoClicks), 0, 'revealing touch controls must not exit fullscreen or focus the video');
      await page.touchscreen.tap(rect.x + rect.width / 2, rect.y + rect.height / 2);
      assert.equal(await page.locator('#tile').getAttribute('data-controls-visible'), 'false');
      for (const width of [320, 390, 640]) {
        await page.setViewportSize({ width, height: 800 });
        await page.evaluate(() => window.dxStreamControls.destroy());
        await page.setContent(fixture(false, true));
        await page.addScriptTag({ content: script });
        const actions = await page.locator('#tile button,#tile input').evaluateAll(nodes => nodes.map(node => {
          const r = node.getBoundingClientRect();
          return { left: r.left, right: r.right, top: r.top, bottom: r.bottom };
        }));
        for (const action of actions) assert.ok(action.left >= 0 && action.right <= width, `coarse ${width}`);
        for (let i = 0; i < actions.length; i++) for (let j = i + 1; j < actions.length; j++) {
          const a = actions[i], b = actions[j];
          const overlap = Math.min(a.right, b.right) - Math.max(a.left, b.left) > 1 && Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 1;
          assert.equal(overlap, false, `touch targets overlap at ${width}`);
        }
      }
    } finally { await touch.close(); }
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });

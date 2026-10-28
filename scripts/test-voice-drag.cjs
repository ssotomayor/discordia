const assert = require('node:assert/strict');
const fs = require('node:fs');
const { chromium } = require('playwright');
const script = fs.readFileSync('client/assets/voice-drag.js', 'utf8');
const css = fs.readFileSync('client/assets/voice-drag.css', 'utf8');

async function fixture(page) {
  await page.setContent(`<style>
    body{margin:0;--accent:#e8933a;--panel-solid:#222;--text:white;--panel2:#333}
    #rail{width:300px;height:320px;overflow:auto;margin:20px} [data-voice-channel]{min-height:100px}
    [data-voice-pubkey]{height:40px;user-select:none} img{width:24px;height:24px} ${css}
  </style><div id="rail">
    <div data-voice-channel="source"><button>Source</button>
      <div id="member" data-voice-pubkey="target" data-voice-from="source" data-voice-name="Target" data-voice-movable="true">
        <img src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='24' height='24'%3E%3Crect width='24' height='24' fill='red'/%3E%3C/svg%3E"><span>Target</span><button id="control">Volume</button>
      </div>
      <div id="protected" data-voice-pubkey="owner" data-voice-movable="false">Owner</div>
    </div><div data-voice-channel="destination"><button id="destination">Destination</button><div>Another occupant</div></div>
    <div id="text" style="height:80px">Text channel</div>
    <div style="height:220px"></div><div data-voice-channel="scrolled"><button>Scrolled destination</button></div>
  </div>`);
  await page.evaluate(() => {
    window.moves = []; window.profileClicks = 0; window.reorders = 0;
    window.dioxus = { send: value => { if (value) moves.push(value); } };
    member.onclick = () => profileClicks++;
    rail.onpointerdown = () => reorders++;
  });
  await page.evaluate(source => (0, eval)(`(${source})`)('rail'), script);
}
async function point(page, selector) {
  const box = await page.locator(selector).boundingBox();
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}
async function begin(page, selector = '#member span') {
  const start = await point(page, selector);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
}
async function over(page, selector = '#destination') {
  const end = await point(page, selector);
  await page.mouse.move(end.x, end.y, { steps: 10 });
}
async function clean(page) {
  assert.equal(await page.locator('.dxf-voice-drag-ghost').count(), 0);
  assert.equal(await page.locator('[data-voice-drop-target]').count(), 0);
}

(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: process.env.BROWSER_PATH || undefined });
  try {
    const page = await browser.newPage({ viewport: { width: 700, height: 600 } });
    for (const source of ['#member span', '#member img']) {
      await fixture(page);
      await begin(page, source);
      await over(page);
      assert.equal(await page.locator('[data-voice-drop-target]').getAttribute('data-voice-channel'), 'destination');
      assert.equal(await page.locator('.dxf-voice-drag-ghost').innerText(), 'Target');
      await page.mouse.up();
      assert.deepEqual(await page.evaluate(() => moves), [{ pubkey: 'target', from_channel_id: 'source', channel_id: 'destination' }]);
      assert.equal(await page.evaluate(() => profileClicks), 0);
      assert.equal(await page.evaluate(() => reorders), 0);
      await clean(page);
    }
    await fixture(page);
    await page.locator('#member span').click();
    assert.equal(await page.evaluate(() => profileClicks), 1);
    assert.deepEqual(await page.evaluate(() => moves), []);
    for (const source of ['#protected', '#control']) {
      await fixture(page); await begin(page, source); await over(page); await page.mouse.up();
      assert.deepEqual(await page.evaluate(() => moves), []); await clean(page);
      assert.equal(await page.evaluate(() => reorders), 0);
    }
    for (const action of ['same', 'text', 'outside', 'escape', 'blur', 'cancel', 'removed']) {
      await fixture(page); await begin(page); await over(page);
      if (action === 'same') await over(page, '#member span');
      if (action === 'text') await over(page, '#text');
      if (action === 'outside') await page.mouse.move(500, 450);
      if (action === 'escape') await page.keyboard.press('Escape');
      if (action === 'blur') await page.evaluate(() => dispatchEvent(new Event('blur')));
      if (action === 'cancel') await page.evaluate(() => document.dispatchEvent(new PointerEvent('pointercancel', { pointerId: 1 })));
      if (action === 'removed') await page.locator('#member').evaluate(node => node.remove());
      await page.mouse.up();
      assert.deepEqual(await page.evaluate(() => moves), [], action); await clean(page);
    }
    await fixture(page);
    await page.evaluate(source => (0, eval)(`(${source})`)('rail'), script);
    await begin(page); await over(page); await page.mouse.up();
    assert.equal(await page.evaluate(() => moves.length), 1, 'reinstall must not duplicate sends');
    await begin(page); await over(page);
    await page.evaluate(() => window.__dxfVoiceDrags.get('rail')());
    await clean(page);
    await page.mouse.up();
    assert.equal(await page.evaluate(() => moves.length), 1, 'dispose must remove handlers');
    await fixture(page); await begin(page);
    await page.mouse.move(100, 332, { steps: 10 });
    await page.waitForFunction(() => rail.scrollTop > 180);
    await page.mouse.up(); await clean(page);
    await fixture(page); await begin(page); await over(page);
    await page.locator('#rail').evaluate(node => node.remove());
    await page.waitForFunction(() => !window.__dxfVoiceDrags.has('rail'));
    await page.mouse.up(); await clean(page);
    assert.deepEqual(await page.evaluate(() => moves), [], 'unmount cancels the drag');
    console.log('Voice drag: avatar/name drops, profile clicks, controls, protected users, cancellation, cleanup, reinstall and autoscroll passed.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });

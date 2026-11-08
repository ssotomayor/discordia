const assert = require('node:assert/strict');
const fs = require('node:fs');
const {chromium} = require('playwright');

const script = fs.readFileSync('client/assets/mobile-pager.js', 'utf8')
  .replace('__PAGER_ID__', '"pager"');
const css = fs.readFileSync('client/assets/mobile.css', 'utf8');

(async () => {
  const browser = await chromium.launch({
    headless: true,
    ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ? {executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH} : {}),
  });
  try {
    for (const [dm, reducedMotion] of [[true, 'no-preference'], [false, 'no-preference'], [true, 'reduce']]) {
      const page = await browser.newPage({
        viewport: {width: 400, height: 700}, isMobile: true, hasTouch: true, reducedMotion,
      });
      await page.setContent(`<meta name="viewport" content="width=device-width,initial-scale=1">
        <style>${css}
          body {margin:0} #pager {height:600px;flex:none} button {width:180px;height:48px}
        </style>
        <button id="back">Conversations</button>
        <div id="pager" class="dxf-mobile-pager" data-mobile-tab="0" data-mobile-dm="${dm}">
          <div data-mobile-panel="channels"><button id="dm">Selected DM</button></div>
          <div data-mobile-panel="chat"><textarea aria-label="Draft">Keep this draft</textarea></div>
          <div data-mobile-panel="members">Members</div>
        </div>`);
      await page.evaluate(() => {
        window.events = [];
        window.dioxus = {send(event) {
          window.events.push(event);
          const pager = document.getElementById('pager');
          if (event.type === 'select' && event.from === Number(pager.dataset.mobileTab)) {
            pager.dataset.mobileTab = event.index;
          }
        }};
        document.getElementById('dm').onclick = () => {
          document.getElementById('pager').dataset.mobileTab = '1';
        };
        document.getElementById('back').onclick = () => {
          document.getElementById('pager').dataset.mobileTab = '0';
        };
      });
      await page.evaluate(source => { void new Function(`return (async () => {${source}})()`)(); }, script);
      await page.waitForFunction(() => window.__dxfMobilePagers?.has('pager'));
      const durations = [];
      for (let attempt = 0; attempt < 10; attempt++) {
        await page.locator('#back').tap();
        await page.waitForFunction(() => Math.abs(document.getElementById('pager').scrollLeft) <= 1);
        const began = Date.now();
        await page.locator('#dm').tap();
        if (attempt === 0) {
          await page.evaluate(() => document.getElementById('pager').dispatchEvent(new Event('scrollend')));
        }
        await page.waitForFunction(() => {
          const pager = document.getElementById('pager');
          return pager.dataset.mobileTab === '1' && Math.abs(pager.scrollLeft - pager.clientWidth) <= 1;
        });
        durations.push(Date.now() - began);
        assert.equal(await page.getByRole('textbox').inputValue(), 'Keep this draft');
      }
      assert.equal(await page.evaluate(() => window.events.filter(event => event.type === 'select').length), 0);
      durations.sort((a, b) => a - b);
      console.log(`dm=${dm}, ${reducedMotion}: 10 single-tap reopens; tap-to-page p50=${durations[5]}ms max=${durations[9]}ms (synthetic desktop Chromium)`);
      await page.evaluate(() => window.__dxfMobilePagers.get('pager')());
      await page.close();
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });

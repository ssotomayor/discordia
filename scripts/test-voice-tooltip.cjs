const assert = require('node:assert/strict');
const fs = require('node:fs');
const { chromium } = require('playwright');

const source = fs.readFileSync('client/src/features/channels.rs', 'utf8').split('fn VoiceSignal(')[1];
const style = source.match(/style: "([^"]+)"/)[1];
const script = fs.readFileSync('client/assets/voice-tooltip.js', 'utf8');

(async () => {
  const browser = await chromium.launch({
    headless: true,
    ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH } : {}),
  });
  try {
    const page = await browser.newPage();
    for (const width of [320, 768, 1280]) {
      await page.setViewportSize({ width, height: 600 });
      await page.setContent(`<style>
        body { margin:0; }
        #rail { position:fixed;left:0;top:0;width:144px;height:600px;background:#333;z-index:500; }
        #panel { position:absolute;left:144px;top:100px;width:220px;height:300px;overflow:auto;transform:translateZ(0); }
        #anchor { display:block;position:relative;margin:120px 0 0 100px;width:14px;height:14px; }
        #tip { padding:8px;background:#111;color:white;font:11px monospace;box-sizing:border-box; }
      </style><div id="rail"></div><div id="panel"><span id="anchor">▥
        <span id="tip" role="tooltip" popover="manual" style="${style}">
          Connection: Excellent<br>Server: 47 ms<br>Packet loss: 0.0%<br>Audio: 3 kbps (target: 64 kbps)
        </span></span><div style="height:700px"></div></div>`);
      await page.evaluate(`(${script})('tip')`);
      const check = async () => {
        const result = await page.evaluate(() => {
          const tip = document.getElementById('tip');
          const r = tip.getBoundingClientRect();
          tip.style.pointerEvents = 'auto';
          const hit = document.elementFromPoint(r.left + 5, r.top + 5);
          tip.style.pointerEvents = 'none';
          return { open: tip.matches(':popover-open'), left:r.left, right:r.right,
            top:r.top, bottom:r.bottom, visible:tip.contains(hit) };
        });
        assert.ok(result.open && result.visible, `occluded tooltip at ${width}: ${JSON.stringify(result)}`);
        assert.ok(result.left >= 8 && result.right <= width - 8, JSON.stringify(result));
        assert.ok(result.top >= 8 && result.bottom <= 592, JSON.stringify(result));
      };
      await check();
      await page.evaluate(() => { document.getElementById('panel').scrollTop = 35; });
      await page.waitForTimeout(50);
      await check();
      await page.evaluate(() => { document.getElementById('tip').textContent = 'Long diagnostic '.repeat(150); });
      await page.waitForTimeout(50);
      await check();
      await page.evaluate(() => document.getElementById('tip').remove());
      await page.waitForTimeout(20);
      assert.equal(await page.locator(':popover-open').count(), 0);
      console.log(`Voice tooltip clipping, stacking, viewport, updates and removal: ${width}px passed`);
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });

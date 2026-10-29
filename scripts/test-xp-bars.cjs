const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const lock = fs.readFileSync('Cargo.lock', 'utf8');
const version = lock.match(/name = "dioxus-interpreter-js"\r?\nversion = "([^"]+)"/)[1];
const registry = path.join(process.env.CARGO_HOME || path.join(os.homedir(), '.cargo'), 'registry', 'src');
const source = fs.readdirSync(registry).map(dir => path.join(registry, dir, `dioxus-interpreter-js-${version}`, 'src', 'js', 'set_attribute.js')).find(file => fs.existsSync(file));
assert.ok(source, 'Run cargo fetch before checking the pinned Dioxus style updater');
const interpreter = fs.readFileSync(source, 'utf8').replace(/export\s*\{\s*setAttributeInner\s*\}\s*;?/g, '');
const styles = ['channels', 'profiles'].map(name => {
  const rust = fs.readFileSync(`client/src/features/${name}.rs`, 'utf8');
  const style = rust.match(/style: "([^"\n]*width: \{xp_pct\}%;[^"\n]*)"/);
  assert.ok(style, `${name}: XP fill style not found`);
  return { name, style: style[1] };
});

(async () => {
  const browser = await chromium.launch({ executablePath: process.env.BROWSER_PATH || undefined, headless: true });
  try {
    const page = await browser.newPage();
    await page.setContent('<style>:root{--accent:#e4b658}#track{width:300px;height:8px}#fill{height:8px}</style><span id="name">Before rename</span><div id="track"><div id="fill"></div></div>');
    await page.addScriptTag({ content: interpreter });
    for (const { name, style } of styles) {
      const fill = page.locator('#fill');
      await fill.evaluate((node, value) => node.setAttribute('style', value), style.replace('{xp_pct}', '20'));
      for (const pct of [20, 30, 45, 45, 70, 90]) {
        await page.locator('#name').evaluate((node, value) => { node.textContent = `Renamed at ${value}%`; }, pct);
        const actual = await fill.evaluate((node, { value }) => {
          setAttributeInner(node, 'style', value, '');
          return { image: getComputedStyle(node).backgroundImage, width: node.getBoundingClientRect().width };
        }, { value: style.replace('{xp_pct}', String(pct)) });
        assert.match(actual.image, /linear-gradient/, `${name}: XP bar vanished after rename/message update at ${pct}%`);
        assert.ok(Math.abs(actual.width - 3 * pct) < .1, `${name}: progress width became stale`);
      }
    }
    console.log('XP bars keep their paint and width through renames and successive awards using the pinned Dioxus updater');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });

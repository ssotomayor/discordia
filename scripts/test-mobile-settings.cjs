const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const dialog = fs.readFileSync('client/src/features/settings_dialog.rs', 'utf8');
const mobile = fs.readFileSync('client/src/features/settings_mobile.rs', 'utf8');
const audio = fs.readFileSync('client/src/features/settings_audio.rs', 'utf8');
const catalog = JSON.parse(fs.readFileSync('client/locales/messages.json', 'utf8'));
const css = ['tailwind.css', 'mobile.css', 'settings.css'].map(file => fs.readFileSync(`client/assets/${file}`, 'utf8')).join('\n');
const cls = (source, prefix) => {
  const result = source.match(new RegExp(`class: "(${prefix}[^"\n]*)"`));
  assert.ok(result, `missing production class ${prefix}`);
  return result[1];
};
const fixture = (language, large) => {
  const text = key => language < 0 ? key : catalog[key]?.[language] || key;
  const button = (label, active = false) => `<button type="button"${active ? ' aria-current="page"' : ''}><span class="dxf-mobile-settings-icon"></span><span>${text(label)}</span></button>`;
  return `<style>${css}
  :root{--panel-solid:#17150f;--panel2:#1b1813;--border:#3a342b;--text:#f2ece3;--text-muted:#9a9287;--text-dim:#857d72;--accent:#e6ba54}
  body{margin:0;color:var(--text)}
  ${large ? '.dxf-settings-content{font-size:20px}.dxf-mobile-settings-nav button{font-size:17px}.dxf-settings-audio-tabs button{font-size:18px}' : ''}
  </style><body class="dxf-platform-mobile dxf-ui">
  <div class="${cls(dialog, 'dxf-settings-backdrop')}"><div class="${cls(dialog, 'dxf-settings dxf-modal-in')}">
  <div class="${cls(dialog, 'dxf-settings-heading')}"><span>${text('Settings')}</span><button aria-label="${text('Close')}">×</button></div>
  <div class="${cls(dialog, 'dxf-settings-body')}"><div class="${cls(dialog, 'dxf-settings-content')}">
  <nav class="${cls(mobile, 'dxf-settings-audio-tabs')}">${['Microphone','Output','App sounds'].map(key => `<button type="button" aria-pressed="${key === 'Microphone'}">${text(key)}</button>`).join('')}</nav>
  <label class="dxf-mobile-settings-field"><span>${text('Theme')}</span><select aria-label="${text('Theme')}"><option>Una opción de dispositivo con un nombre muy largo</option></select></label>
  <details class="dxf-mic-advanced" open><summary>${text('Advanced voice settings')}</summary>
  <label class="flex items-center gap-2"><span>${text('Listening with')}</span><select><option>${text('Headphones')}</option></select></label>
  <div class="dxf-settings-control"><label>${text('Sensitivity')}</label><output>40%</output><input type="range"></div></details>
  <div class="${cls(audio, 'dxf-audio-sounds')}">${Array.from({length:12},(_,i)=>`<label class="dxf-audio-sound-row"><span>${i%2 ? 'Espectadores que dejan de ver tu transmisión de pantalla' : text('App sounds')}</span><input aria-label="Sound ${i}" type="checkbox"></label>`).join('')}</div>
  </div></div><nav class="${cls(mobile, 'dxf-mobile-settings-nav')}">${['General','Audio','Video','Support'].map(key=>button(key,key==='Audio')).join('')}</nav>
  </div></div></body>`;
};

(async () => {
  const browser = await chromium.launch({executablePath:process.env.BROWSER_PATH || undefined,headless:true});
  try {
    const page = await browser.newPage();
    let checked = 0;
    for (const [width,height] of [[320,640],[360,740],[412,900],[740,360]]) {
      await page.setViewportSize({width,height});
      for (const language of [-1,0,1,2,3]) for (const large of [false,true]) {
        await page.setContent(fixture(language,large));
        const geometry = await page.evaluate(() => {
          const bounds = node => {const r=node.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:r.width,height:r.height};};
          const nav=document.querySelector('.dxf-mobile-settings-nav');
          const content=document.querySelector('.dxf-settings-content');
          const overflow=[...document.querySelectorAll('button,label,select,summary')].filter(node=>node.scrollWidth>node.clientWidth+2).map(node=>node.tagName+':'+node.className);
          return {nav:bounds(nav),content:bounds(content),buttons:[...nav.children].map(bounds),fields:[...document.querySelectorAll('select')].map(bounds),overflow,documentOverflow:document.documentElement.scrollWidth>innerWidth,scrollable:content.scrollHeight>content.clientHeight};
        });
        const context=`${width}x${height}, language=${language}, large=${large}`;
        assert.equal(geometry.documentOverflow,false,context);
        assert.deepEqual(geometry.overflow,[],context);
        assert.ok(geometry.content.height>0,context);
        assert.ok(geometry.content.bottom<=geometry.nav.top+1,context);
        assert.ok(geometry.nav.bottom<=height,context);
        assert.ok(geometry.scrollable,context);
        for(const rect of geometry.buttons) {assert.ok(rect.width>=44,context);assert.ok(rect.height>=44,context);}
        for(const rect of geometry.fields) assert.ok(rect.height>=44,context);
        await page.locator('.dxf-settings-content').evaluate(node=>{node.scrollTop=node.scrollHeight;});
        const after=await page.locator('.dxf-mobile-settings-nav').boundingBox();
        assert.equal(after.y,geometry.nav.top,context);
        checked++;
      }
    }
    console.log(`Mobile settings geometry: ${checked} viewport/language/text-size combinations passed`);
  } finally {
    await browser.close();
  }
})().catch(error=>{console.error(error);process.exitCode=1;});

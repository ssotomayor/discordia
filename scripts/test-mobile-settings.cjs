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
  <nav class="${cls(mobile, 'dxf-settings-audio-tabs')}">${['Microphone','Sound'].map(key => `<button type="button" aria-pressed="${key === 'Microphone'}">${text(key)}</button>`).join('')}</nav>
  <label class="dxf-mobile-settings-field"><span>${text('Theme')}</span><select aria-label="${text('Theme')}"><option>Una opción de dispositivo con un nombre muy largo</option></select></label>
  <details class="dxf-mic-advanced" open><summary>${text('Advanced voice settings')}</summary>
  <label class="flex items-center gap-2"><span>${text('Listening with')}</span><select><option>${text('Headphones')}</option></select></label>
  <div class="dxf-settings-control"><label>${text('Sensitivity')}</label><output>40%</output><input type="range"></div></details>
  <div class="${cls(audio, 'dxf-audio-sounds')}">${Array.from({length:12},(_,i)=>`<label class="dxf-audio-sound-row"><span>${i%2 ? 'Espectadores que dejan de ver tu transmisión de pantalla' : text('App sounds')}</span><input aria-label="Sound ${i}" type="checkbox"></label>`).join('')}</div>
  </div></div><nav class="${cls(mobile, 'dxf-mobile-settings-nav')}">${['General','Audio','Video','App'].map(key=>button(key,key==='Audio')).join('')}</nav>
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
    const camera = fs.readFileSync('client/src/features/camera_setup.rs', 'utf8');
    const startClass = camera.match(/id: "camera-setup-start",[\s\S]*?class: "([^"]+)"/)[1];
    const themes = [...fs.readFileSync('client/src/app.rs', 'utf8').matchAll(/ThemeDef \{\s*id: "([^"]+)",[\s\S]*?vars: "([\s\S]*?)",\s*\}/g)];
    assert.ok(themes.length >= 10);
    await page.setViewportSize({width:360,height:740});
    let contrastCases = 0;
    for (const [,name,vars] of themes) for (const accent of ['', '#ffffff', '#000000']) for (const disabled of [false,true]) {
      const theme = vars.replace(/\\\r?\n\s*/g, '');
      const override = accent ? `--accent:${accent};--accent-soft:color-mix(in srgb,${accent} 14%,transparent);` : '';
      const label = catalog[disabled ? 'Starting…' : 'Start camera'][0];
      await page.setContent(`<style>${css}</style><div class="dxf-platform-mobile" style="${theme}${override}background-color:var(--panel-solid)"><div id="camera-setup-dialog"><button class="${startClass}" ${disabled ? 'disabled' : ''}>${label}</button></div></div>`);
      const rendered = await page.locator('button').evaluate(button => {
        const canvas = document.createElement('canvas'); canvas.width=canvas.height=1;
        const ctx = canvas.getContext('2d');
        ctx.fillStyle='#fff';ctx.fillRect(0,0,1,1);
        for (const node of [document.documentElement,document.body,button.parentElement.parentElement,button.parentElement,button]) {
          ctx.fillStyle=getComputedStyle(node).backgroundColor;ctx.fillRect(0,0,1,1);
        }
        const background=Array.from(ctx.getImageData(0,0,1,1).data).slice(0,3);
        ctx.fillStyle=getComputedStyle(button).color;ctx.fillRect(0,0,1,1);
        const foreground=Array.from(ctx.getImageData(0,0,1,1).data).slice(0,3);
        const luminance = rgb => rgb.map(c=>c/255).map(c=>c<=0.04045?c/12.92:((c+0.055)/1.055)**2.4).reduce((sum,c,i)=>sum+c*[0.2126,0.7152,0.0722][i],0);
        const [dark,light]=[luminance(background),luminance(foreground)].sort((a,b)=>a-b);
        return {contrast:(light+0.05)/(dark+0.05),height:button.getBoundingClientRect().height,text:button.textContent};
      });
      const context=`camera start: theme=${name}, accent=${accent || 'default'}, disabled=${disabled}`;
      assert.ok(rendered.contrast>=4.5, `${context}: contrast ${rendered.contrast}`);
      assert.ok(rendered.height>=44,context);
      assert.equal(rendered.text,label,context);
      contrastCases++;
    }
    console.log(`Camera start label contrast: ${contrastCases} theme/accent/state combinations passed`);
  } finally {
    await browser.close();
  }
})().catch(error=>{console.error(error);process.exitCode=1;});

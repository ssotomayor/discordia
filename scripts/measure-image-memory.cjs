const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawn, spawnSync } = require('node:child_process');
const { performance } = require('node:perf_hooks');

const root = path.resolve(__dirname, '..');
const out = path.join(root, 'target/image-memory');
const value = name => process.argv[process.argv.indexOf(name) + 1];
const variant = value('--variant');
const exe = path.resolve(value('--exe'));
const repetitions = Number(value('--runs') || 5);
const fixtures = path.join(out, 'fixtures');
const resultFile = path.join(out, 'results.json');
const durableFile = path.join(root, 'docs/RAM_PHASE8_RESULTS.json');
fs.mkdirSync(out, { recursive: true });
const result = fs.existsSync(resultFile) ? JSON.parse(fs.readFileSync(resultFile)) : {
  sourceCommit: spawnSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).stdout.trim(),
  scope: 'Real Dioxus/ImageAssets/WebView2 component fixture; not full Discordia or network transfer; GPU process private commit is not VRAM',
  fixtures: fs.readdirSync(fixtures).sort().map(name => { const bytes = fs.readFileSync(path.join(fixtures, name)); return { name, bytes: bytes.length, sha256: crypto.createHash('sha256').update(bytes).digest('hex') }; }),
  binaries: {}, samples: [],
};
result.binaries[variant] = { sha256: crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex'), bytes: fs.statSync(exe).size };
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
let sequence = 0;
async function rpc(url, method, params = {}) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const id = ++sequence;
    const timer = setTimeout(() => { ws.close(); reject(new Error(`CDP timeout: ${method}`)); }, 60000);
    ws.onopen = () => ws.send(JSON.stringify({ id, method, params }));
    ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP socket failed')); };
    ws.onmessage = event => {
      const message = JSON.parse(event.data);
      if (message.id !== id) return;
      clearTimeout(timer); ws.close();
      if (message.error || message.result?.exceptionDetails) reject(new Error(JSON.stringify(message)));
      else resolve(message.result);
    };
  });
}
const evaluate = async (url, expression) => (await rpc(url, 'Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result?.value;
const save = () => { const json = JSON.stringify(result, null, 2); fs.writeFileSync(resultFile, json); fs.writeFileSync(durableFile, json + '\n'); };

async function measure(pid, tag) {
  const file = path.join(out, `${variant}-${tag}-memory.json`);
  const response = spawnSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(root, 'scripts/measure-ui-memory.ps1'), '-TargetPid', String(pid), '-DurationSeconds', '2', '-DescendantsOnly', '-OutputJson', file], { encoding: 'utf8' });
  fs.writeFileSync(file + '.log', response.stdout + response.stderr);
  if (response.status !== 0) throw new Error('Profiler failed');
  const raw = JSON.parse(fs.readFileSync(file, 'utf8').replace(/^\uFEFF/, ''));
  const host = raw.Processes.find(p => p.PID === pid);
  const renderers = raw.Processes.filter(p => p.Role.includes('Renderer'));
  const gpu = raw.Processes.filter(p => p.Role.includes('GPU'));
  if (!host || !renderers.length || !gpu.length) throw new Error('Incomplete topology');
  const sum = (ps, key) => ps.reduce((n, p) => n + p[key], 0);
  return { summary: raw.Summary, processCount: raw.Processes.length, rustPrivateMiB: host.PrivateBytesMB,
    rendererPrivateMiB: sum(renderers, 'PrivateBytesMB'), gpuPrivateMiB: sum(gpu, 'PrivateBytesMB'),
    rustCpuPercent: host.CpuPercent, rendererCpuPercent: sum(renderers, 'CpuPercent'), gpuCpuPercent: sum(gpu, 'CpuPercent'),
    processes: raw.Processes.map(({ Role, PrivateBytesMB, WorkingSetMB, CpuPercent }) => ({ Role, PrivateBytesMB, WorkingSetMB, CpuPercent })) };
}

async function run(runNumber) {
  if (result.samples.some(s => s.variant === variant && s.run === runNumber && s.scenario === 'cleared')) return;
  const folder = path.join(out, `${variant}-${runNumber}`);
  if (fs.existsSync(folder)) throw new Error(`Preserve incomplete attempt: ${folder}`);
  fs.mkdirSync(folder);
  const port = 19348;
  try { await fetch(`http://127.0.0.1:${port}/json/version`); throw new Error('CDP port occupied'); }
  catch (error) { if (error.message === 'CDP port occupied') throw error; }
  const log = fs.openSync(path.join(folder, 'app.log'), 'w');
  const child = spawn(exe, [], { cwd: root, env: { ...process.env, IMAGE_PROBE_DATA: fixtures,
    WEBVIEW2_USER_DATA_FOLDER: path.join(folder, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${port}` }, stdio: ['ignore', log, log] });
  fs.closeSync(log);
  child.on('error', console.error);
  fs.writeFileSync(path.join(folder, 'owner.json'), JSON.stringify({ pid: child.pid, exe }));
  try {
    let url;
    const started = performance.now();
    while (performance.now() - started < 60000) {
      if (child.exitCode !== null) throw new Error(`Probe exited ${child.exitCode}`);
      try {
        const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        const page = pages.find(p => p.type === 'page' && p.url.includes('dioxus'));
        if (page && await evaluate(page.webSocketDebuggerUrl, '!!document.querySelector("#probe")')) { url = page.webSocketDebuggerUrl; break; }
      } catch {}
      await sleep(100);
    }
    if (!url) throw new Error('Probe UI not ready');
    result.browser = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    const settle = async () => evaluate(url, `(async()=>{await Promise.all([...document.querySelectorAll('#images img')].map(i=>i.decode()));await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));return [...document.querySelectorAll('#images img')].map(i=>({w:i.naturalWidth,h:i.naturalHeight,width:i.width,height:i.height,src:i.getAttribute('src')}));})()`);
    const click = async selector => evaluate(url, `document.querySelector(${JSON.stringify(selector)}).click()`);
    const waitState = async (n, cycle) => {
      for (let i = 0; i < 400; i++) {
        if (await evaluate(url, `document.querySelector('#probe')?.dataset.scenario===${JSON.stringify(String(n))}&&document.querySelector('#probe')?.dataset.cycle===${JSON.stringify(String(cycle))}`)) return;
        await sleep(25);
      }
      throw new Error('Fixture transition timed out');
    };
    const cache = () => evaluate(url, "fetch('/probe-stats/current').then(r=>r.json())");
    for (const [n, name] of [[1, 'chat'], [2, 'high_resolution'], [3, 'avatars_emojis'], [4, 'animated'], [5, 'channel_switches'], [6, 'dm_switches'], [7, 'full_viewer'], [8, 'minimize_restore'], [9, 'background'], [0, 'cleared']]) {
      const begin = performance.now();
      await click(`#scenario-${n}`); await waitState(n, 0);
      let images = await settle();
      if ([5, 6, 7, 9].includes(n)) for (let c = 1; c <= 6; c++) { await click('#next'); await waitState(n, c); images = await settle(); }
      const loadMs = performance.now() - begin;
      await sleep(1000);
      const cacheStats = await cache();
      if (runNumber === 1 && n !== 0) {
        const shot = await rpc(url, 'Page.captureScreenshot', { format: 'png' });
        fs.writeFileSync(path.join(folder, `${name}.png`), Buffer.from(shot.data, 'base64'));
      }
      if (n === 8) {
        const windowCommand = code => spawnSync('powershell.exe', ['-NoProfile', '-Command', `Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class ProbeWindow{[DllImport("user32.dll")]public static extern bool ShowWindow(IntPtr h,int c);}';$p=Get-Process -Id ${child.pid};[ProbeWindow]::ShowWindow($p.MainWindowHandle,${code})`], { encoding: 'utf8' });
        windowCommand(6); await sleep(1000);
        result.samples.push({ variant, run: runNumber, scenario: 'minimized', cache: await cache(), ...await measure(child.pid, `${runNumber}-minimized`) }); save();
        windowCommand(9); await sleep(1000);
      }
      const metric = await measure(child.pid, `${runNumber}-${name}`);
      const imageMetadata = images.map(({src, ...dimensions}) => dimensions);
      result.samples.push({ variant, run: runNumber, scenario: name, loadMs, imageCount: images.length, images: imageMetadata, cache: cacheStats, ...metric }); save();
      console.log(variant, runNumber, name, metric.summary.TotalPrivateCommitMb, cacheStats.encodedBytes);
    }
  } finally {
    const cleanup = spawnSync('powershell.exe', ['-NoProfile', '-Command', `$ErrorActionPreference='Stop';$all=Get-CimInstance Win32_Process;$owned=@(${child.pid});do{$count=$owned.Count;$owned=@($owned+@($all|Where-Object{$_.ParentProcessId -in $owned}|Select-Object -ExpandProperty ProcessId)|Sort-Object -Unique)}while($count -lt $owned.Count);$records=@($all|Where-Object{$_.ProcessId -in $owned});$p=Get-Process -Id ${child.pid} -ErrorAction SilentlyContinue;if($p -and $p.Path -eq '${exe.replaceAll("'", "''")}'){[void]$p.CloseMainWindow();if(-not $p.WaitForExit(5000)){taskkill /PID ${child.pid} /T /F}};Start-Sleep -Seconds 2;foreach($r in $records){$q=Get-CimInstance Win32_Process -Filter ('ProcessId='+$r.ProcessId);if($q -and $q.CreationDate -eq $r.CreationDate){Stop-Process -Id $r.ProcessId -Force}};Start-Sleep -Milliseconds 500;foreach($r in $records){$q=Get-CimInstance Win32_Process -Filter ('ProcessId='+$r.ProcessId);if($q -and $q.CreationDate -eq $r.CreationDate){throw ('Owned PID remains: '+$r.ProcessId)}};'Owned tree exited'`], { encoding: 'utf8' });
    fs.writeFileSync(path.join(folder, 'cleanup.log'), cleanup.stdout + cleanup.stderr);
    if (cleanup.status !== 0) throw new Error('Owned process cleanup failed');
    await sleep(1000);
  }
}
(async () => {
  if (process.platform !== 'win32') throw new Error('Windows required');
  if (!variant || !fs.existsSync(exe)) throw new Error('--variant and --exe required');
  for (let n = 1; n <= repetitions; n++) await run(n);
})().catch(error => { console.error(error); process.exitCode = 1; });

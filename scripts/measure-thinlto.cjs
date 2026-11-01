const fs = require('node:fs');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');
const { performance } = require('node:perf_hooks');

const root = path.resolve(__dirname, '..');
const out = path.join(root, 'target', 'thinlto-stage4');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const resultsFile = path.join(out, 'runs.json');
const results = fs.existsSync(resultsFile) ? JSON.parse(fs.readFileSync(resultsFile, 'utf8')) : [];

async function rpc(url, expression) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => { ws.close(); reject(new Error('CDP timeout')); }, 2000);
    ws.onopen = () => ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression, returnByValue: true } }));
    ws.onmessage = event => {
      const msg = JSON.parse(event.data);
      if (msg.id !== 1) return;
      clearTimeout(timer);
      ws.close();
      resolve(msg.result?.result?.value);
    };
    ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP connection failed')); };
  });
}

async function run(profile, run) {
  if (results.some(result => result.profile === profile && result.run === run)) return;
  const tag = `${profile}-${run}`;
  const folder = path.join(out, tag);
  if (fs.existsSync(folder)) throw new Error(`Preserve the incomplete attempt before retrying: ${folder}`);
  fs.mkdirSync(folder, { recursive: true });
  const exe = path.join(root, 'target', profile, 'Discordia.exe');
  const port = 19347;
  try {
    await fetch(`http://127.0.0.1:${port}/json/version`);
    throw new Error(`Debugging port ${port} already in use`);
  } catch (error) {
    if (error.message.includes('already in use')) throw error;
  }
  const log = fs.openSync(path.join(folder, 'app.log'), 'w');
  const started = performance.now();
  const child = spawn(exe, [], {
    cwd: root,
    env: { ...process.env, DISCORDIA_CONFIG_DIR: path.join(folder, 'config'), DISCORDIA_VAULT: 'file',
      WEBVIEW2_USER_DATA_FOLDER: path.join(folder, 'webview'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${port}` },
    stdio: ['ignore', log, log], windowsHide: false,
  });
  fs.closeSync(log);
  fs.writeFileSync(path.join(folder, 'owner.json'), JSON.stringify({ pid: child.pid, exe }));
  child.on('error', error => console.error(error));
  let runError;
  try {
    let ready;
    while (performance.now() - started < 30000) {
      if (child.exitCode !== null) throw new Error(`App exited: ${child.exitCode}`);
      try {
        const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
        const page = pages.find(p => p.type === 'page' && p.url.includes('dioxus'));
        if (page) {
          ready = await rpc(page.webSocketDebuggerUrl,
            `document.querySelector('#main')?.hasAttribute('data-ready') ? ({title:document.title,text:document.querySelector('.dxf-ui')?.innerText?.slice(0,240),width:innerWidth,height:innerHeight}) : null`);
          if (ready) break;
        }
      } catch {}
      await sleep(25);
    }
    if (!ready) throw new Error('No rendered UI within 30 seconds');
    const startupMs = performance.now() - started;
    const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    await sleep(15000);
    const measurement = path.join(folder, 'memory.json');
    const ps = spawnSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
      path.join(root, 'scripts', 'measure-ui-memory.ps1'), '-TargetPid', String(child.pid),
      '-DurationSeconds', '5', '-DescendantsOnly', '-OutputJson', measurement], { encoding: 'utf8' });
    fs.writeFileSync(path.join(folder, 'profiler.log'), ps.stdout + ps.stderr);
    if (ps.status !== 0 || !fs.existsSync(measurement)) throw new Error('Profiler failed');
    const memory = JSON.parse(fs.readFileSync(measurement, 'utf8').replace(/^\uFEFF/, ''));
    if (!memory.Processes.some(p => p.PID === child.pid) || !memory.Processes.some(p => p.Role.includes('Renderer')))
      throw new Error('Incomplete process topology');
    results.push({ profile, run, startupMs, ready, version, ...memory });
    fs.writeFileSync(resultsFile, JSON.stringify(results, null, 2));
    console.log(tag, startupMs.toFixed(1), memory.Summary);
  } catch (error) {
    runError = error;
    throw error;
  } finally {
    const cleanup = spawnSync('powershell.exe', ['-NoProfile', '-Command',
      `$ErrorActionPreference='Stop';
      $p=Get-Process -Id ${child.pid} -ErrorAction SilentlyContinue;
      if ($p -and $p.Path -eq '${exe.replaceAll("'", "''")}') {
        $all=Get-CimInstance Win32_Process; $owned=@(${child.pid});
        do { $previous=$owned.Count; $owned=@($owned + @($all | Where-Object { $_.ParentProcessId -in $owned } | Select-Object -ExpandProperty ProcessId) | Sort-Object -Unique) } while ($owned.Count -gt $previous);
        $records=@($all | Where-Object { $_.ProcessId -in $owned });
        $records | Select-Object ProcessId,ParentProcessId,Name,CreationDate | ConvertTo-Json;
        [void]$p.CloseMainWindow();
        if (-not $p.WaitForExit(3000)) { taskkill /PID ${child.pid} /T /F | Out-String | Write-Output }
        Start-Sleep -Seconds 2;
        foreach ($record in $records) {
          $remaining=Get-CimInstance Win32_Process -Filter ('ProcessId='+$record.ProcessId);
          if ($remaining -and $remaining.CreationDate -eq $record.CreationDate) {
            Stop-Process -Id $record.ProcessId -Force;
          }
        }
        Start-Sleep -Milliseconds 500;
        foreach ($record in $records) {
          $remaining=Get-CimInstance Win32_Process -Filter ('ProcessId='+$record.ProcessId);
          if ($remaining -and $remaining.CreationDate -eq $record.CreationDate) { throw ('Owned process still running: '+$record.ProcessId) }
        }
        'Owned process tree exited.'
      }
      'Cleanup verification complete.'`], { encoding: 'utf8' });
    fs.writeFileSync(path.join(folder, 'cleanup.log'), JSON.stringify({status:cleanup.status,error:String(cleanup.error)}) + '\n' + cleanup.stdout + cleanup.stderr);
    await sleep(2000);
    if (cleanup.status !== 0 && !runError) throw new Error(`Cleanup failed; see ${folder}`);
    if (child.exitCode === null) throw new Error(`Owned process ${child.pid} still running`);
  }
}

(async () => {
  if (process.platform !== 'win32') throw new Error('Windows required');
  if (results.length < 10) {
    const active = spawnSync('powershell.exe', ['-NoProfile', '-Command',
      `@(Get-CimInstance Win32_Process -Filter "Name='Discordia.exe'") | Select-Object ProcessId,ExecutablePath | ConvertTo-Json`], { encoding: 'utf8' });
    if (active.status !== 0) throw new Error(`Process preflight failed: ${active.stderr || active.error}`);
    if (active.stdout.trim()) throw new Error(`Discordia's single-instance guard requires closing existing instances first: ${active.stdout}`);
    for (const profile of ['release', 'release-lto']) {
      if (!fs.existsSync(path.join(root, 'target', profile, 'Discordia.exe'))) throw new Error(`Build ${profile} first`);
    }
  }
  for (let n = 1; n <= 5; n++) {
    for (const profile of n % 2 ? ['release', 'release-lto'] : ['release-lto', 'release']) await run(profile, n);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });

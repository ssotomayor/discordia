import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const exec = promisify(execFile);

export function androidPackage(value = 'com.discordia.app') {
  if (!['com.discordia.app', 'com.discordia.app.debug'].includes(value)) throw new Error('Choose the Discordia release or debug package');
  return value;
}


export function processCounters(text) {
  const end = text.lastIndexOf(')');
  const fields = text.slice(end + 2).trim().split(/\s+/);
  const ticks = Number(fields[11]) + Number(fields[12]);
  if (end < 0 || !Number.isFinite(ticks)) throw new Error('Invalid process counters');
  return ticks;
}

export function frameCounters(text) {
  const get = re => { const match = text.match(re); return match ? Number(match[1]) : null; };
  return { frames: get(/Total frames rendered:\s*(\d+)/), janky: get(/Janky frames:\s*(\d+)/) };
}

export function frameDelta(before, after) {
  if (Object.values(before).some(v => v === null) || Object.values(after).some(v => v === null)) return null;
  const frames = after.frames - before.frames, janky = after.janky - before.janky;
  if (frames <= 0 || janky < 0 || janky > frames) return null;
  return { frames, janky, jankyPercent: 100 * janky / frames };
}

export function memoryCounters(text) {
  const total = text.match(/TOTAL PSS:\s*(\d+).*?TOTAL RSS:\s*(\d+)/s);
  if (total) return { pssKiB: Number(total[1]), rssKiB: Number(total[2]) };
  const legacy = text.match(/^\s*TOTAL\s+(\d+)/m);
  return { pssKiB: legacy ? Number(legacy[1]) : null, rssKiB: null };
}

export async function settingsScenario() {
  const samples = [], pause = ms => new Promise(resolve => setTimeout(resolve, ms));
  async function transition(button, ready) {
    if (!button) throw new Error('Settings control unavailable');
    const start = performance.now();
    return new Promise((resolve, reject) => {
      let done = false;
      const observer = new MutationObserver(finish);
      const timer = setTimeout(() => { observer.disconnect(); reject(new Error('Settings transition timed out')); }, 5000);
      function finish() {
        if (done || !ready()) return;
        done = true; observer.disconnect(); clearTimeout(timer);
        requestAnimationFrame(() => requestAnimationFrame(() => resolve(performance.now() - start)));
      }
      observer.observe(document.body, { childList: true, subtree: true, attributes: true });
      button.click(); finish();
    });
  }
  const open = () => document.querySelector('button[title="Ajustes"],button[title="Settings"],button[title="Configurações"]');
  const present = () => !!document.querySelector('.dxf-settings');
  const initiallyOpen = present();
  try {
    if (present()) await transition(document.querySelector('.dxf-settings-heading button'), () => !present());
    for (let i = 0; i < 10; i++) {
      await pause(300);
      const opened = await transition(open(), present);
      await pause(300);
      const closed = await transition(document.querySelector('.dxf-settings-heading button'), () => !present());
      samples.push({ opened, closed });
    }
    return { metric: 'Synthetic click to DOM change plus two animation frames; excludes physical touch dispatch', samples };
  } finally {
    if (present() !== initiallyOpen) await transition(initiallyOpen ? open() : document.querySelector('.dxf-settings-heading button'), () => present() === initiallyOpen);
  }
}

export async function navigationScenario() {
  const nav = () => document.querySelector('.dxf-mobile-nav');
  if (!nav()) throw new Error('Mobile navigation unavailable');
  const buttons = () => [...nav().querySelectorAll('button')];
  const initial = buttons().findIndex(button => button.classList.contains('font-semibold'));
  const samples = [], chats = [];
  async function switchTo(index) {
    const start = performance.now();
    await new Promise((resolve, reject) => {
      let done = false;
      const observer = new MutationObserver(finish);
      const timer = setTimeout(() => { observer.disconnect(); reject(new Error('Tab transition timed out')); }, 5000);
      function finish() {
        if (done || !buttons()[index].classList.contains('font-semibold')) return;
        done = true; observer.disconnect(); clearTimeout(timer);
        requestAnimationFrame(() => requestAnimationFrame(resolve));
      }
      observer.observe(document.body, { childList: true, subtree: true, attributes: true });
      buttons()[index].click(); finish();
    });
    const chat = document.querySelector('#dxf-chat-scroll');
    if (index === 1) chats.push({ node: chat, bridge: window.__dxfChatScrollMeasure });
    return { tab: index === 0 ? 'channels' : 'chat', ms: performance.now() - start, nodes: document.querySelectorAll('*').length };
  }
  try {
    for (let i = 0; i < 5; i++) {
      await new Promise(resolve => setTimeout(resolve, 300)); samples.push(await switchTo(1));
      await new Promise(resolve => setTimeout(resolve, 300)); samples.push(await switchTo(0));
    }
    return { metric: 'Synthetic click to selected tab plus two animation frames', samples,
      chatNodeReused: chats.length > 1 && !!chats[0].node && chats.every(chat => chat.node === chats[0].node),
      scrollBridgeReused: chats.length > 1 && !!chats[0].bridge && chats.every(chat => chat.bridge === chats[0].bridge) };
  } finally {
    if (initial >= 0 && initial !== 0) await switchTo(initial);
  }
}

const probe = `(() => {
  if (window.__discordiaPerfProbe) throw new Error('A performance probe is already running');
  const frames = [], tasks = [], events = [], observers = [];
  let previous, raf, active = true;
  const loop = now => { if (previous !== undefined) frames.push(now - previous); previous = now; if (active) raf = requestAnimationFrame(loop); };
  raf = requestAnimationFrame(loop);
  const observe = (type, values) => {
    if (!PerformanceObserver.supportedEntryTypes.includes(type)) return false;
    const observer = new PerformanceObserver(list => { for (const entry of list.getEntries()) values.push(entry.duration); });
    observer.observe({ type, buffered: false, durationThreshold: 16 }); observers.push(observer); return true;
  };
  const supported = { longtask: observe('longtask', tasks), event: observe('event', events) };
  let hidden = document.hidden;
  const visibility = () => { if (document.hidden) hidden = true; };
  document.addEventListener('visibilitychange', visibility);
  window.__discordiaPerfProbe = () => {
    active = false; cancelAnimationFrame(raf); observers.forEach(o => o.disconnect());
    document.removeEventListener('visibilitychange', visibility); delete window.__discordiaPerfProbe;
    const summary = values => { values.sort((a,b) => a-b); return { count: values.length, p50: values.length ? values[Math.floor((values.length-1)*.5)] : null, p95: values.length ? values[Math.floor((values.length-1)*.95)] : null, max: values.length ? values[values.length-1] : null }; };
    return { supported, hiddenDuringSample: hidden, frameIntervalMs: summary(frames), framesOver50ms: frames.filter(v => v > 50).length, longTasksMs: summary(tasks), eventDurationMs: summary(events) };
  };
  return true;
})()`;

export async function cdp(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
  let next = 0;
  const pending = new Map();
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data), waiting = pending.get(message.id);
    if (!waiting) return;
    pending.delete(message.id); clearTimeout(waiting.timer);
    if (message.error || message.result?.exceptionDetails) waiting.reject(new Error('WebView evaluation failed'));
    else waiting.resolve(message.result?.result?.value);
  });
  return {
    evaluate(expression, timeoutMs = 10000) {
      return new Promise((resolve, reject) => {
        const id = ++next;
        const timer = setTimeout(() => { pending.delete(id); reject(new Error('WebView timeout')); }, timeoutMs);
        pending.set(id, { resolve, reject, timer });
        socket.send(JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, returnByValue: true, awaitPromise: true } }));
      });
    },
    close() { socket.close(); },
  };
}

async function main() {
  const args = process.argv.slice(2), options = {};
  for (let i = 0; i < args.length; i += 2) {
    if (!['--adb', '--seconds', '--label', '--out', '--serial', '--scenario', '--package'].includes(args[i]) || !args[i+1]) throw new Error('Usage: node scripts/measure-android.mjs [--adb PATH] [--seconds 30] [--label idle] [--out target/android-perf] [--serial SERIAL] [--scenario passive|settings|navigation] [--package com.discordia.app.debug]');
    options[args[i].slice(2)] = args[i+1];
  }
  const seconds = Number(options.seconds ?? 30);
  if (!Number.isFinite(seconds) || seconds < 5 || seconds > 300) throw new Error('Use 5–300 seconds');
  if (options.scenario && !['passive', 'settings', 'navigation'].includes(options.scenario)) throw new Error('Unknown scenario');
  if (options.scenario && options.scenario !== 'passive' && seconds < 15) throw new Error('Interactive scenario needs at least 15 seconds');
  const adbPath = options.adb ?? 'adb', pkg = androidPackage(options.package);
  const adb = async (...command) => (await exec(adbPath, [...(options.serial ? ['-s', options.serial] : []), ...command], { maxBuffer: 8*1024*1024, timeout: 20000 })).stdout.trim();
  const devices = (await adb('devices')).split(/\r?\n/).filter(line => /\tdevice$/.test(line));
  if (!options.serial && devices.length !== 1) throw new Error(`Expected one authorized USB device; found ${devices.length}`);
  const pid = await adb('shell', 'pidof', pkg);
  if (!/^\d+$/.test(pid)) throw new Error('Open Discordia before measuring');
  const hz = Number(await adb('shell', 'getconf', 'CLK_TCK'));
  if (!Number.isFinite(hz) || hz <= 0) throw new Error('Cannot determine CPU clock ticks');
  const directory = path.resolve(options.out ?? 'target/android-perf', new Date().toISOString().replace(/[:.]/g, '-') + '-' + (options.label ?? 'sample').replace(/[^a-zA-Z0-9_-]/g, '_'));
  await mkdir(directory, { recursive: true });
  const shell = (...command) => adb('shell', ...command);
  const counters = async () => {
    const start = performance.now();
    try {
      const output = await shell('run-as', pkg, 'cat', `/proc/${pid}/stat`);
      return { ticks: processCounters(output), time: (start + performance.now()) / 2000 };
    } catch (error) {
      report.cpuUnavailable = 'App process counters require a debuggable build and run-as access';
      return { ticks: null, time: performance.now() / 1000 };
    }
  };
  const report = { label: options.label ?? 'sample', seconds, cpuScope: 'main app process; 100% = one CPU core; excludes isolated WebView renderer', memoryScope: 'dumpsys meminfo package; renderer may be reported separately', frameScope: 'Android window counters; WebView rAF/event metrics reported separately', samples: [] };
  let client, port, scenario;
  try {
    report.model = await shell('getprop', 'ro.product.model');
    report.sdk = await shell('getprop', 'ro.build.version.sdk');
    report.package = (await shell('dumpsys', 'package', pkg)).split(/\r?\n/).filter(line => /versionCode=|versionName=|lastUpdateTime=|flags=\[/.test(line)).map(line => line.trim());
    report.thermalBefore = (await shell('dumpsys', 'thermalservice')).split(/\r?\n/).filter(line => /Thermal Status|mStatus=/.test(line));
    const framesBefore = frameCounters(await shell('dumpsys', 'gfxinfo', pkg));
    try {
      const socket = (await shell('cat', '/proc/net/unix')).split(/\r?\n/).map(line => line.split('@')[1]).find(name => name === `webview_devtools_remote_${pid}`);
      if (!socket) throw new Error('Debug WebView socket unavailable');
      port = await adb('forward', 'tcp:0', `localabstract:${socket}`);
      const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      const page = pages.find(page => page.type === 'page' && /dioxus\.index\.html/.test(page.url));
      if (!page) throw new Error('App WebView unavailable');
      client = await cdp(page.webSocketDebuggerUrl);
      await client.evaluate(probe);
    } catch (error) { report.webviewUnavailable = error.message; }
    if (options.scenario && options.scenario !== 'passive') {
      if (!client || report.webviewUnavailable) throw new Error('Interactive scenario requires the debug WebView inspector');
      const run = options.scenario === 'settings' ? settingsScenario : navigationScenario;
      scenario = client.evaluate(`(${run.toString()})()`, 30000).then(value => { report[options.scenario] = value; }, error => { report.scenarioError = error.message; process.exitCode = 1; });
    }
    let previous = await counters();
    const until = performance.now() + seconds * 1000;
    console.log(`Sampling ${report.model} for ${seconds}s. Perform the scenario now; no messages are sent by the probe.`);
    while (performance.now() < until) {
      await new Promise(resolve => setTimeout(resolve, Math.min(2000, Math.max(1, until-performance.now()))));
      if (await shell('pidof', pkg) !== pid) throw new Error('App restarted; sample is incomplete');
      const current = await counters();
      const elapsed = current.time - previous.time;
      const sample = { timeSeconds: current.time, cpuPercent: elapsed > 0 && current.ticks !== null && previous.ticks !== null ? 100*(current.ticks-previous.ticks)/hz/elapsed : null };
      const memory = await shell('dumpsys', 'meminfo', pkg);
      Object.assign(sample, memoryCounters(memory));
      report.samples.push(sample);
      previous = current;
    }
    report.windowFrames = frameDelta(framesBefore, frameCounters(await shell('dumpsys', 'gfxinfo', pkg)));
    report.thermalAfter = (await shell('dumpsys', 'thermalservice')).split(/\r?\n/).filter(line => /Thermal Status|mStatus=/.test(line));
  } catch (error) { report.error = error.message; process.exitCode = 1; }
  finally {
    if (scenario) await scenario;
    if (client) {
      try { report.webview = await client.evaluate('window.__discordiaPerfProbe?.()'); } catch (error) { report.webviewError = error.message; }
      client.close();
    }
    if (port) {
      try { await adb('forward', '--remove', `tcp:${port}`); } catch { report.forwardCleanupFailed = true; }
    }
    await writeFile(path.join(directory, 'report.json'), JSON.stringify(report, null, 2) + '\n');
  }
  console.log(JSON.stringify(report, null, 2));
  console.log(`Saved ${directory}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { cdp, androidPackage } from './measure-android.mjs';

const exec = promisify(execFile), options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  if (!['--adb', '--runs', '--kind', '--label', '--out', '--package'].includes(process.argv[i]) || !process.argv[i+1]) throw new Error('Use --adb PATH --runs 3 --kind cold|warm --label NAME --out DIRECTORY [--package com.discordia.app.debug]');
  options[process.argv[i].slice(2)] = process.argv[i+1];
}
const runs = Number(options.runs ?? 3), kind = options.kind ?? 'cold';
if (!Number.isInteger(runs) || runs < 1 || runs > 10 || !['cold', 'warm'].includes(kind)) throw new Error('Use 1–10 runs and cold|warm');
const adb = async (...args) => (await exec(options.adb ?? 'adb', args, { timeout: 30000, maxBuffer: 1024*1024 })).stdout.trim();
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const pkg = androidPackage(options.package);
const report = { kind, runs, label: options.label ?? 'startup', definition: 'Host elapsed until debug inspector observes visible .dxf-ui and main[data-ready]; includes inspector overhead; separate from network reconnection', samples: [] };
const directory = path.resolve(options.out ?? 'target/android-perf', new Date().toISOString().replace(/[:.]/g, '-') + '-' + report.label.replace(/[^a-zA-Z0-9_-]/g, '_'));
await mkdir(directory, { recursive: true });
try {
  const devices = (await adb('devices')).split(/\r?\n/).filter(line => /\tdevice$/.test(line));
  if (devices.length !== 1) throw new Error('Connect exactly one authorized Android device');
  report.model = await adb('shell', 'getprop', 'ro.product.model');
  report.sdk = await adb('shell', 'getprop', 'ro.build.version.sdk');
  for (let i = 0; i < runs; i++) {
    if (kind === 'cold') await adb('shell', 'am', 'force-stop', pkg);
    else await adb('shell', 'input', 'keyevent', 'KEYCODE_HOME');
    await pause(500);
    const start = performance.now();
    const launch = await adb('shell', 'am', 'start', '-W', '-n', `${pkg}/dev.dioxus.main.MainActivity`);
    let port, client;
    try {
      const pid = await adb('shell', 'pidof', pkg);
      if (!/^\d+$/.test(pid)) throw new Error('App did not start');
      for (let poll = 0; poll < 100; poll++) {
        const sockets = await adb('shell', 'cat', '/proc/net/unix');
        if (sockets.includes(`@webview_devtools_remote_${pid}`)) break;
        await pause(100);
      }
      port = await adb('forward', 'tcp:0', `localabstract:webview_devtools_remote_${pid}`);
      const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      const page = pages.find(page => page.type === 'page' && /dioxus\.index\.html/.test(page.url));
      if (!page) throw new Error('App inspector unavailable');
      client = await cdp(page.webSocketDebuggerUrl);
      let ready = false;
      for (let poll = 0; poll < 200; poll++) {
        ready = await client.evaluate('!!document.querySelector("#main[data-ready] .dxf-ui") && !document.hidden && !document.querySelector("#startup")');
        if (ready) break;
        await pause(50);
      }
      if (!ready) throw new Error('Visible interface did not become ready');
      const nativeTime = launch.match(/TotalTime:\s*(\d+)/);
      const sample = { run: i+1, visibleUiMs: performance.now()-start, nativeActivityMs: nativeTime ? Number(nativeTime[1]) : null, launchState: launch.match(/LaunchState:\s*(\S+)/)?.[1] ?? null };
      report.samples.push(sample); console.log(JSON.stringify(sample));
    } finally {
      client?.close();
      if (port) await adb('forward', '--remove', `tcp:${port}`);
    }
    await pause(1000);
  }
} catch (error) { report.error = error.message; process.exitCode = 1; }
finally { await writeFile(path.join(directory, 'report.json'), JSON.stringify(report, null, 2)+'\n'); console.log(`Saved ${directory}`); }

#!/usr/bin/env node
// Render one state of an app board (the design's .dc.html mockups) to PNG,
// cropped to the window's content (under the mock title bar), so it lines up
// with the egui window's screenshots (scripts/ui-parity/parity.py).
//
// usage: node board-shot.mjs --board App-Main [--props '{"state":"Bypass"}']
//          [--theme dark|light] [--size default|minimum] [--dpr 1|2]
//          [--at MS] [--ticks N] [--eval JS] [--measure 'sel;sel'] --out FILE.png
//   --ticks N   the boards' live levels (50 ms intervals) are held and stepped
//               N times just before the shot, so the columns are exactly the
//               first N clip frames: the egui scene prefills the same N.
//   --measure   print each matching element's rect, relative to the crop.
// The design folder is $HM_DESIGN (default $HOME/hm-v1/design). Needs
// google-chrome and network for React and the Geist web fonts.
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const args = process.argv.slice(2);
const opt = { props: '{}', theme: null, size: null, dpr: 1, at: 6000, ticks: null, evals: [], measure: null, out: null, board: null, crop: 'body', keep: false };
for (let i = 0; i < args.length; i++) {
  const a = args[i], v = () => args[++i];
  if (a === '--board') opt.board = v();
  else if (a === '--props') opt.props = v();
  else if (a === '--theme') opt.theme = v();
  else if (a === '--size') opt.size = v();
  else if (a === '--dpr') opt.dpr = +v();
  else if (a === '--at') opt.at = +v();
  else if (a === '--ticks') opt.ticks = +v();
  else if (a === '--eval') opt.evals.push(v());
  else if (a === '--measure') opt.measure = v();
  else if (a === '--crop') opt.crop = v();
  else if (a === '--out') opt.out = v();
  else if (a === '--keep') opt.keep = true;
  else { console.error('unknown argument', a); process.exit(2); }
}
if (!opt.board || !opt.out) { console.error('need --board and --out'); process.exit(2); }

const DESIGN = process.env.HM_DESIGN || path.join(os.homedir(), 'hm-v1/design');
const BOARDS = path.join(DESIGN, 'hm-canvas/project');
const file = opt.board.endsWith('.html') ? opt.board : path.join(BOARDS, opt.board.replace(/\.dc$/, '') + '.dc.html');
const url = (p) => pathToFileURL(path.join(DESIGN, p)).href;

// ---- the harness page (gen.mjs, with absolute URLs) ----
function parse(f) {
  const src = readFileSync(f, 'utf8');
  const xdc = src.slice(src.indexOf('<x-dc>') + 6, src.indexOf('</x-dc>'));
  const hs = xdc.indexOf('<helmet>'), he = xdc.indexOf('</helmet>');
  const helmet = hs < 0 ? '' : xdc.slice(hs + 8, he);
  const body = hs < 0 ? xdc : xdc.slice(0, hs) + xdc.slice(he + 9);
  const sm = /<script type="text\/x-dc"[^>]*data-props='([^']*)'[^>]*>([\s\S]*?)<\/script>/.exec(src);
  const dp = JSON.parse(sm[1].replace(/&#39;/g, "'").replace(/&amp;/g, '&'));
  const props = {};
  for (const [k, v] of Object.entries(dp)) if (!k.startsWith('$') && v && 'default' in v) props[k] = v.default;
  return { helmet, body, cls: sm[2], props };
}
const board = parse(file);
const props = Object.assign({}, board.props, JSON.parse(opt.props));
if (opt.theme) props.theme = opt.theme;
if (opt.size) props.size = opt.size === 'minimum' ? 'Minimum 720 × 520' : 'Default 960 × 640';
// Hold the 50/100 ms level intervals; window.__hmTick(n) steps them.
const HOLD = `<script>(function(){var held=[];var si=window.setInterval;
window.setInterval=function(f,ms){if(ms===50||ms===100){held.push(f);return 900000+held.length;}return si.apply(window,arguments);};
var ci=window.clearInterval;window.clearInterval=function(id){if(id>900000){held[id-900001]=null;return;}return ci.apply(window,arguments);};
window.__hmTick=function(n){for(var k=0;k<n;k++)held.forEach(function(f){if(f)f();});return held.filter(Boolean).length;};})();</script>`;
const html = `<!doctype html><html lang="en"><head><meta charset="utf-8">
${opt.ticks != null ? HOLD : ''}
<link rel="stylesheet" href="${url('hm-canvas/project/ds/hushmic/tokens.css')}">
<link rel="stylesheet" href="${url('hm-ds/project/components/bundle.css')}">
<script src="https://cdn.jsdelivr.net/npm/react@18.3.1/umd/react.production.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/react-dom@18.3.1/umd/react-dom.production.min.js"></script>
<script src="${url('hm-ds/project/components/bundle.js')}"></script>
${board.helmet}
<style>body{margin:0}</style>
</head><body>
<template id="dc">${board.body}</template>
<div id="root"></div>
<script src="${url('team/lab/ux/dc-shim.js')}"></script>
<script>${board.cls}
DCMount(Component, ${JSON.stringify(props)});</script>
</body></html>`;
const scratch = mkdtempSync(path.join(os.tmpdir(), 'hm-board-'));
const page = path.join(scratch, 'board.html');
writeFileSync(page, html);

// ---- headless Chrome over the DevTools protocol ----
const W = 1120, H = 820;
const chrome = spawn('nice', ['-n', '10', 'google-chrome', '--headless=new', '--remote-debugging-port=0',
  `--user-data-dir=${path.join(scratch, 'profile')}`, '--no-first-run', '--no-default-browser-check', '--hide-scrollbars',
  '--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--mute-audio',
  '--allow-file-access-from-files', '--disable-lcd-text', `--window-size=${W},${H}`, 'about:blank'], { stdio: 'ignore' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cleanup = () => { try { chrome.kill('SIGKILL'); } catch {} if (!opt.keep) try { rmSync(scratch, { recursive: true, force: true }); } catch {} };
process.on('exit', cleanup);
setTimeout(() => { console.error('timeout'); process.exit(1); }, 90000 + opt.at).unref();
const portFile = path.join(scratch, 'profile', 'DevToolsActivePort');
for (let i = 0; i < 300 && !existsSync(portFile); i++) await sleep(50);
await sleep(100);
const [port, wsPath] = readFileSync(portFile, 'utf8').trim().split('\n');
const ws = new WebSocket(`ws://127.0.0.1:${port}${wsPath}`);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let id = 0; const pending = new Map(); const listeners = [];
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.j(new Error(JSON.stringify(m.error))) : p.r(m.result); }
  else listeners.forEach((f) => f(m));
};
const send = (method, params = {}, sessionId) => new Promise((r, j) => { const i = ++id; pending.set(i, { r, j }); ws.send(JSON.stringify({ id: i, method, params, sessionId })); });
const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
const S = (m, p) => send(m, p, sessionId);
listeners.push((m) => {
  if (m.sessionId !== sessionId) return;
  if (m.method === 'Runtime.exceptionThrown') console.error('EXCEPTION:', m.params.exceptionDetails?.exception?.description || m.params.exceptionDetails?.text);
});
await S('Page.enable'); await S('Runtime.enable');
await S('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: opt.dpr, mobile: false });
const loaded = new Promise((r) => listeners.push((m) => { if (m.sessionId === sessionId && m.method === 'Page.loadEventFired') r(); }));
await S('Page.navigate', { url: pathToFileURL(page).href });
await Promise.race([loaded, sleep(30000)]);
await sleep(opt.at);
const ev = async (js) => {
  const r = await S('Runtime.evaluate', { expression: js, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error('eval: ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
  return r.result?.value;
};
await ev('document.fonts.ready.then(() => true)');
for (const js of opt.evals) console.log('eval:', JSON.stringify(await ev(js)));
if (opt.ticks != null) { await ev(`window.__hmTick(${opt.ticks})`); await sleep(400); }
const sel = opt.crop === 'window' ? '.hm-app-win' : opt.crop === 'none' ? null : '.hm-app-body';
const crop = sel ? await ev(`(() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect(); return [r.left, r.top, r.width, r.height]; })()`) : [0, 0, W, H];
if (opt.measure) {
  const out = await ev(`(() => { const o = ${JSON.stringify(crop)}; const res = {};
    for (const s of ${JSON.stringify(opt.measure.split(';'))}) res[s] = [...document.querySelectorAll(s)].map((e) => {
      const r = e.getBoundingClientRect(); return [+(r.left - o[0]).toFixed(2), +(r.top - o[1]).toFixed(2), +r.width.toFixed(2), +r.height.toFixed(2)]; });
    return res; })()`);
  console.log(JSON.stringify(out));
}
const { data } = await S('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false, clip: { x: crop[0], y: crop[1], width: crop[2], height: crop[3], scale: 1 } });
writeFileSync(opt.out, Buffer.from(data, 'base64'));
console.log('wrote', opt.out, crop.map((v) => Math.round(v)).join(','));
ws.close();
process.exit(0);

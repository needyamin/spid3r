//! The `spid3r-control` desktop panel (feature `gui`).
//!
//! A tao/wry window over the same op layer the CLI uses ([`super::ops`]).
//! Deliberately NOT the `spid3r gui` program machinery: the panel is a fixed
//! native surface and never runs `.matrix` source in-process. Language
//! actions (run/check/build/…) go through [`super::lang`], which launches the
//! real `spid3r` binary as a child process, so a faulty program cannot crash
//! the panel. Its IPC surface is a closed set of ops.
//!
//! Protocol: the page calls `window.spid3r.emit(json)` where json is
//! `{op, id, params}`; the Rust side dispatches through
//! [`super::ops::dispatch`] and replies with
//! `window.__spid3rReply(id, {ok, data|error})`.
//!
//! Layout contract: the window has a minimum size (860×580 logical) and the
//! page is responsive below that — a two-pane panel must not lose its
//! controls when the user shrinks or DPI-scales the window.

use std::borrow::Cow;
use std::path::PathBuf;

use tao::event_loop::{ControlFlow, EventLoopBuilder};
use wry::http::Response as HttpResponse;

use super::ops;

/// Wakes the event loop when the page posts an op.
pub enum Msg {
    Op(String),
    Reply(String),
}

const INIT_SCRIPT: &str = r#"window.spid3r = {
  emit: function (json) {
    try { window.ipc.postMessage(json); } catch (e) {}
  }
};"#;

/// The control-panel page. Single file, zero external assets: the installer
/// payload stays opaque and `file://`/custom-protocol quirks never matter.
/// `##` closes the raw string because the CSS contains `#` selectors.
const CONTROL_PAGE: &str = r##"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Spid3r Control Panel</title>
<style>
  :root { --bg:#f4f6f5; --panel:#ffffff; --ink:#1c2422; --muted:#66736e;
          --line:#dfe6e2; --accent:#0f8a5f; --accent-soft:#e3f3ec;
          --warn:#b45309; --warn-soft:#fef3e2; --bad:#b91c1c; --bad-soft:#fdecec; }
  * { box-sizing: border-box; }
  html, body { height: 100%; }
  body { margin:0; font:14px/1.5 "Segoe UI",system-ui,sans-serif;
         background:var(--bg); color:var(--ink); display:flex; overflow:hidden; }
  nav { width:186px; min-width:186px; background:var(--panel);
        border-right:1px solid var(--line); padding:16px 0;
        display:flex; flex-direction:column; }
  .brand { font-weight:700; font-size:16px; padding:0 18px 14px; }
  .brand small { display:block; color:var(--muted); font-weight:400; font-size:11px; }
  nav button { display:block; width:100%; text-align:left; padding:9px 18px;
        border:0; background:none; cursor:pointer; font-size:13.5px; color:var(--ink); }
  nav button:hover { background:var(--accent-soft); }
  nav button.on { background:var(--accent-soft); color:var(--accent);
        font-weight:600; border-right:3px solid var(--accent); }
  .nav-danger { margin-top:auto; padding:12px 18px 0; border-top:1px solid var(--line);
        font-size:11.5px; color:var(--muted); }
  main { flex:1; overflow:auto; padding:22px 26px; min-width:0; }
  h1 { font-size:19px; margin:0 0 4px; }
  .sub { color:var(--muted); margin:0 0 18px; font-size:12.5px; max-width:860px; }
  .card { background:var(--panel); border:1px solid var(--line); border-radius:10px;
          padding:14px 16px; margin-bottom:12px; max-width:980px; }
  .row { display:flex; justify-content:space-between; gap:14px; padding:5px 0;
         flex-wrap:wrap; }
  .row .k { color:var(--muted); flex-shrink:0; }
  .row .v { text-align:right; word-break:break-all; min-width:0; }
  .ok { color:var(--accent); font-weight:600; }
  .bad { color:var(--bad); font-weight:600; }
  .warn { color:var(--warn); font-weight:600; }
  .banner { border-radius:10px; padding:12px 16px; margin-bottom:14px;
        font-weight:600; max-width:980px; }
  .banner.all-safe { background:var(--accent-soft); color:var(--accent);
        border:1px solid #bfe3d4; }
  .banner.some-open { background:var(--warn-soft); color:var(--warn);
        border:1px solid #f3ddb8; }
  .sec-title { font-size:13px; font-weight:700; text-transform:uppercase;
        letter-spacing:.04em; color:var(--muted); margin:20px 0 8px; }
  .item { display:flex; align-items:flex-start; gap:14px; padding:11px 0;
        border-bottom:1px solid var(--line); flex-wrap:wrap; }
  .item:last-child { border-bottom:0; }
  .item .info { flex:1 1 320px; min-width:0; }
  .item .info b { font-size:14px; }
  .item .info .desc { color:var(--muted); font-size:12.5px; margin-top:2px; }
  .item .info .key { font:11px Consolas,monospace; color:var(--muted); opacity:.8; }
  .item .ctl { display:flex; align-items:center; gap:10px; flex-shrink:0; }
  .state-chip { font-size:11.5px; font-weight:700; padding:3px 9px; border-radius:99px; }
  .state-chip.open { background:var(--bad-soft); color:var(--bad); }
  .state-chip.safe { background:var(--accent-soft); color:var(--accent); }
  /* Toggle switch */
  .switch { position:relative; width:42px; height:23px; flex-shrink:0; }
  .switch input { opacity:0; width:0; height:0; }
  .switch span { position:absolute; inset:0; background:#c3cdc8; border-radius:99px;
        transition:background .18s; cursor:pointer; }
  .switch span::before { content:""; position:absolute; width:17px; height:17px;
        left:3px; top:3px; background:#fff; border-radius:50%; transition:left .18s; }
  .switch input:checked + span { background:var(--accent); }
  .switch input:checked + span::before { left:22px; }
  .switch.dangerous input:checked + span { background:var(--bad); }
  input[type=text], input[type=number] { padding:6px 8px; border:1px solid var(--line);
        border-radius:6px; font:12.5px/1.4 Consolas,monospace; width:220px; max-width:100%; }
  select { padding:6px 8px; border:1px solid var(--line); border-radius:6px;
        font:12.5px/1.4 system-ui; max-width:220px; }
  button.act { background:var(--accent); color:#fff; border:0; border-radius:6px;
        padding:6px 13px; cursor:pointer; font-size:12.5px; }
  button.act:hover { filter:brightness(1.08); }
  button.ghost { background:none; border:1px solid var(--line); color:var(--ink);
        border-radius:6px; padding:5px 11px; cursor:pointer; font-size:12.5px; }
  button.ghost:hover { background:var(--accent-soft); }
  .mod-chip { font-size:11px; color:var(--warn); background:var(--warn-soft);
        border-radius:99px; padding:2px 8px; }
  pre { background:#f0f3f1; border:1px solid var(--line); border-radius:8px;
        padding:12px; font:12px/1.5 Consolas,monospace; overflow:auto;
        max-height:360px; white-space:pre-wrap; }
  .hidden { display:none; }
  textarea { width:100%; min-height:260px; border:1px solid var(--line); border-radius:8px;
        padding:10px; font:12.5px/1.5 Consolas,monospace; resize:vertical; tab-size:4; }
  #toast { position:fixed; bottom:16px; right:16px; background:var(--ink); color:#fff;
        padding:10px 16px; border-radius:8px; font-size:13px; opacity:0;
        transition:opacity .25s; pointer-events:none; max-width:460px;
        word-break:break-word; z-index:10; }
  #toast.show { opacity:.96; }
  #toast.bad { background:var(--bad); }
  @media (max-width: 760px) {
    nav { width:64px; min-width:64px; }
    .brand { font-size:0; padding:0 8px 12px; }
    .brand small { display:none; }
    nav button { font-size:11px; padding:9px 6px; text-align:center; }
    main { padding:16px 14px; }
  }
</style>
</head>
<body>
<nav>
  <div class="brand">Spid3r<small>Control Panel</small></div>
  <button data-tab="overview" class="on">Overview</button>
  <button data-tab="language">Language</button>
  <button data-tab="safety">Safety</button>
  <button data-tab="config">Configuration</button>
  <button data-tab="audit">Logs</button>
  <div class="nav-danger">Every change on this panel is validated against the
    schema and saved to spid3r.ini.</div>
</nav>
<main>
  <section id="tab-overview">
    <h1>Overview</h1>
    <p class="sub">Runtime status and quick operations for this machine.</p>
    <div class="card" id="overview-cards"></div>
    <div class="card" style="display:flex;gap:8px;flex-wrap:wrap">
      <button class="act" onclick="reloadIni()">Reload spid3r.ini</button>
      <button class="ghost" onclick="refreshOverview()">Refresh</button>
    </div>
  </section>
  <section id="tab-language" class="hidden">
    <h1>Language</h1>
    <p class="sub">Drive the Spid3r toolchain: every action runs the real
       <code>spid3r</code> binary (same flags, exit codes and sandbox as the CLI).
       Interactive tools (repl, debug, lsp) stay in a terminal.</p>
    <div class="card" id="lang-info"></div>
    <div class="card">
      <div class="item">
        <div class="info"><b>Project folder</b>
          <div class="desc">Working directory for commands; leave empty for the panel's own.</div></div>
        <div class="ctl"><input type="text" id="lang-cwd" placeholder="C:\path\to\project" style="width:340px">
          <button class="ghost" onclick="langScan()">Scan files</button></div>
      </div>
      <div class="item">
        <div class="info"><b>Source file</b>
          <div class="desc">.matrix / .matrixc path (or pick a scanned one).</div></div>
        <div class="ctl"><select id="lang-pick" style="max-width:260px"><option value="">— scanned files —</option></select>
          <input type="text" id="lang-file" placeholder="src\main.matrix" style="width:260px"></div>
      </div>
      <div class="item">
        <div class="info"><b>Command</b>
          <div class="desc">Extra flags go in the box; put program arguments after <code> -- </code>.</div></div>
        <div class="ctl"><select id="lang-cmd"></select>
          <input type="text" id="lang-args" placeholder="--sandbox --budget 1000000" style="width:260px">
          <button class="act" onclick="langRun()">Run command</button></div>
      </div>
      <div style="display:flex;gap:8px;flex-wrap:wrap;padding-top:6px">
        <button class="ghost" onclick="langQuick('run','--sandbox')">Run (sandboxed)</button>
        <button class="ghost" onclick="langQuick('run','')">Run</button>
        <button class="ghost" onclick="langQuick('check','')">Check</button>
        <button class="ghost" onclick="langQuick('fmt','')">Format</button>
        <button class="ghost" onclick="langQuick('compile','')">Compile</button>
        <button class="ghost" onclick="langQuick('build','--release')">Build exe</button>
        <button class="ghost" onclick="langQuick('test','',true)">Test</button>
        <button class="ghost" onclick="langQuick('modules','--tree')">Modules</button>
        <button class="ghost" onclick="langQuick('init','',true)">Init project</button>
        <button class="ghost" onclick="langQuick('install','',true)">Install deps</button>
        <button class="ghost" onclick="langQuick('info','',true)">Runtime info</button>
        <button class="ghost" onclick="langTerm('repl',true)">Open REPL</button>
        <button class="ghost" onclick="langTerm('debug')">Debug…</button>
        <button class="ghost" onclick="langTerm('gui')">Run as GUI</button>
      </div>
    </div>
    <div class="sec-title">Output <span id="lang-status" class="mod-chip" style="display:none"></span></div>
    <pre id="lang-out">(nothing run yet)</pre>
    <div class="sec-title">Editor</div>
    <div class="card">
      <textarea id="lang-src" spellcheck="false" placeholder="Open the file named above to edit it here…"></textarea>
      <div style="margin-top:8px;display:flex;gap:8px">
        <button class="ghost" onclick="langOpen()">Open</button>
        <button class="act" onclick="langSave()">Save</button>
        <button class="ghost" onclick="langSaveRun()">Save &amp; check</button>
      </div>
    </div>
  </section>
  <section id="tab-safety" class="hidden">
    <h1>Safety switches</h1>
    <p class="sub">What a Spid3r <b>program</b> may do on this machine. "Open"
       means programs can use the capability; "Restricted" means the runtime
       denies it. Changes save to spid3r.ini and apply to newly started runs.</p>
    <div id="safety-banner"></div>
    <div id="safety-list"></div>
  </section>
  <section id="tab-config" class="hidden">
    <h1>Configuration</h1>
    <p class="sub" id="config-sub">spid3r.ini — grouped by section. Boolean and
       enum keys save immediately; text and number keys save on Set. Secret
       values are shown redacted.</p>
    <div style="margin-bottom:10px;display:flex;gap:8px;flex-wrap:wrap">
      <input type="text" id="cfg-filter" placeholder="filter keys…" style="width:260px">
      <button class="ghost" onclick="reloadIni().then(loadConfig)">Reload spid3r.ini</button>
      <button class="ghost" onclick="resetAllConfig()">Reset all to defaults</button>
    </div>
    <div id="config-groups"></div>
  </section>
  <section id="tab-audit" class="hidden">
    <h1>Logs</h1>
    <p class="sub">Inspect <code>spid3r.log</code> (runtime diagnostics, controlled by
       <code>[logging]</code> &amp; <code>[debug]</code> in spid3r.ini).</p>
    <div style="display:flex;gap:8px;margin-bottom:10px;flex-wrap:wrap">
      <button class="act" onclick="loadAudit()">Refresh log</button>
    </div>
    <pre id="audit-text"></pre>
  </section>
</main>
<div id="toast"></div>
<script>
let _id = 0; const _pending = {};
window.spid3r = { emit: (s) => window.ipc.postMessage(s) };
window.__spid3rReply = (id, resp) => {
  const p = _pending[id]; if (!p) return; delete _pending[id];
  resp.ok ? p.res(resp.data) : p.rej(resp.error);
};
function call(op, params = {}) {
  return new Promise((res, rej) => {
    const id = ++_id; _pending[id] = { res, rej };
    window.spid3r.emit(JSON.stringify({ op, id, params }));
  });
}
function toast(msg, isBad) { const t = document.getElementById('toast');
  t.textContent = msg; t.classList.toggle('bad', !!isBad); t.classList.add('show');
  clearTimeout(t._h); t._h = setTimeout(() => t.classList.remove('show'), 4500); }
function esc(s) { const d = document.createElement('div'); d.textContent = s ?? '';
  return d.innerHTML.replace(/"/g, '&quot;').replace(/'/g, '&#39;'); }
function handleErr(e) { toast(String(e), true); }

// ------------------------------------------------------------- overview --
async function refreshOverview() {
  try {
    const s = await call('status');
    const ini = s.ini_exists === 'true'
      ? (s.ini_valid === 'true'
          ? '<span class="ok">valid</span>'
          : '<span class="bad">INVALID</span><br><code>' + esc(s.ini_errors) + '</code>')
      : '<span class="warn">no spid3r.ini yet</span> <button class="ghost" onclick="initIni()">write default</button>' +
        (s.ini_errors ? '<br><span class="bad">' + esc(s.ini_errors) + '</span>' : '');
    document.getElementById('overview-cards').innerHTML =
      '<div class="row"><span class="k">Spid3r</span><span class="v">v' + esc(s.version) +
        ' (language ' + esc(s.language) + ' · env: ' + esc(s.env || 'default') + ')</span></div>' +
      '<div class="row"><span class="k">spid3r.ini</span><span class="v">' + esc(s.ini_path) + ' — ' + ini + '</span></div>' +
      '<div class="row"><span class="k">State dir</span><span class="v">' + esc(s.state_dir) + '</span></div>';
  } catch (e) { handleErr(e); }
}
async function reloadIni() {
  try { const r = await call('config_reload'); toast(r.message); await refreshOverview(); }
  catch (e) { handleErr(e); }
}
async function resetAllConfig() {
  if (!confirm('Reset ALL spid3r.ini keys to their schema defaults?')) return;
  try { const r = await call('config_reset_all', { section: '' }); toast(r.message); await loadConfig(); await refreshOverview(); }
  catch (e) { handleErr(e); }
}
async function initIni() {
  try { const r = await call('ini_init', { force: false }); toast(r.message); await refreshOverview(); }
  catch (e) { handleErr(e); }
}

// --------------------------------------------------------------- safety --
// deny_* keys are SAFE when true (the capability is denied). The other
// safety keys are OPEN when true. Restricting a deny_* key writes "true";
// restricting any other key writes "false".
function denyFamily(key) { return key.startsWith('security.deny_'); }
function restrictValue(key) { return denyFamily(key) ? 'true' : 'false'; }
function openValue(key) { return denyFamily(key) ? 'false' : 'true'; }

async function loadSafety() {
  try {
    const s = await call('safety_overview');
    const open = parseInt(s.open, 10), total = parseInt(s.total, 10);
    document.getElementById('safety-banner').className =
      'banner ' + (open === 0 ? 'all-safe' : 'some-open');
    document.getElementById('safety-banner').innerHTML = open === 0
      ? 'All capabilities restricted — programs run with no machine access beyond the runtime.'
      : open + ' of ' + total + ' capabilities OPEN. Review the switches below.';
    const list = document.getElementById('safety-list');
    list.innerHTML = '';
    for (const row of s.items.split('\n')) {
      if (!row) continue;
      const [key, state, label, consequence, value] = row.split('|');
      const isOpen = state === 'open';
      const card = document.createElement('div');
      card.className = 'card';
      card.innerHTML =
        '<div class="item">' +
          '<div class="info"><b>' + esc(label) + '</b>' +
            '<div class="desc">' + esc(consequence) + '</div>' +
            '<div class="key">' + esc(key) + ' = ' + esc(value) + '</div></div>' +
          '<div class="ctl">' +
            '<span class="state-chip ' + (isOpen ? 'open' : 'safe') + '">' +
              (isOpen ? 'OPEN' : 'RESTRICTED') + '</span>' +
            '<label class="switch' + (isOpen ? ' dangerous' : '') + '">' +
              '<input type="checkbox" data-safety="' + esc(key) + '"' + (isOpen ? ' checked' : '') + '>' +
              '<span title="toggle"></span></label>' +
          '</div></div>';
      list.appendChild(card);
    }
    list.querySelectorAll('input[data-safety]').forEach(cb => {
      cb.addEventListener('change', () => toggleSafety(cb.dataset.safety, cb.checked));
    });
  } catch (e) { handleErr(e); }
}
async function toggleSafety(key, wantOpen) {
  const val = wantOpen ? openValue(key) : restrictValue(key);
  if (wantOpen && !confirm('Open ' + key + '?\n\nPrograms will be able to use this capability.')) {
    loadSafety(); return;
  }
  try {
    const r = await call('config_set', { key, value: val });
    toast(r.message);
    await loadSafety(); await refreshOverview();
  } catch (e) { handleErr(e); await loadSafety(); }
}

// ---------------------------------------------------------------- config --
const SECTION_TITLES = {
  spid3r: 'Core & Scheduler [spid3r]', runtime: 'Execution & Timezone [runtime]',
  net: 'Networking [net]', memory: 'Resource Limits [memory]',
  debug: 'Debugging [debug]', errors: 'Diagnostics [errors]',
  logging: 'Logging [logging]', modules: 'Modules [modules]',
  security: 'Security & Sandbox [security]', packages: 'Package Manager [packages]',
  paths: 'Host Paths [paths]', cache: 'Bytecode Cache [cache]',
};
async function loadConfig() {
  try {
    const filter = document.getElementById('cfg-filter').value.toLowerCase();
    const s = await call('config_list');
    const groups = {};
    for (const row of s.keys.split('\n')) {
      if (!row) continue;
      const [key, kind, value, def, desc, options, reload, frozen] = row.split('|');
      if (filter && !key.toLowerCase().includes(filter) &&
          !desc.toLowerCase().includes(filter)) continue;
      const sec = key.split('.')[0];
      (groups[sec] = groups[sec] || []).push({ key, kind, value, def, desc, options, reload, frozen });
    }
    const root = document.getElementById('config-groups');
    root.innerHTML = '';
    for (const [sec, keys] of Object.entries(groups)) {
      const title = document.createElement('div');
      title.className = 'sec-title';
      title.textContent = SECTION_TITLES[sec] || sec;
      root.appendChild(title);
      const card = document.createElement('div');
      card.className = 'card';
      for (const k of keys) {
        const modified = k.value !== k.def;
        const row = document.createElement('div');
        row.className = 'item';
        let editor;
        if (k.kind === 'Bool') {
          editor = '<label class="switch"><input type="checkbox" data-bool="' + esc(k.key) + '"' +
            (k.value === 'true' ? ' checked' : '') + '><span></span></label>';
        } else if (k.kind === 'Enum' && k.options) {
          editor = '<select data-enum="' + esc(k.key) + '">' +
            k.options.split(',').map(o =>
              '<option' + (o === k.value ? ' selected' : '') + '>' + esc(o) + '</option>').join('') +
            '</select>';
        } else if (k.kind === 'Int') {
          editor = '<input type="number" data-num="' + esc(k.key) + '" value="' + esc(k.value) + '">' +
            '<button class="ghost" data-set="' + esc(k.key) + '">Set</button>';
        } else {
          editor = '<input type="text" data-text="' + esc(k.key) + '" value="' + esc(k.value) + '">' +
            '<button class="ghost" data-set="' + esc(k.key) + '">Set</button>';
        }
        row.innerHTML =
          '<div class="info"><b>' + esc(k.desc) + '</b>' +
            '<div class="key">' + esc(k.key) +
            (k.reload === 'live' ? ' · live' : ' · startup') +
            (k.frozen === 'true' ? ' · host-only' : '') +
            (modified ? ' <span class="mod-chip">modified · default: ' + esc(k.def) +
              ' <a href="#" data-reset="' + esc(k.key) + '">reset</a></span>' : '') +
            '</div></div>' +
          '<div class="ctl">' + editor + '</div>';
        card.appendChild(row);
      }
      root.appendChild(card);
    }
    root.querySelectorAll('input[data-bool]').forEach(cb =>
      cb.addEventListener('change', () => setKey(cb.dataset.bool, cb.checked ? 'true' : 'false')));
    root.querySelectorAll('select[data-enum]').forEach(sel =>
      sel.addEventListener('change', () => setKey(sel.dataset.enum, sel.value)));
    root.querySelectorAll('[data-set]').forEach(btn =>
      btn.addEventListener('click', () => {
        const key = btn.dataset.set;
        const el = root.querySelector('[data-num="' + CSS.escape(key) + '"], [data-text="' + CSS.escape(key) + '"]');
        if (el) setKey(key, el.value);
      }));
    root.querySelectorAll('[data-reset]').forEach(a =>
      a.addEventListener('click', (ev) => {
        ev.preventDefault();
        resetKey(a.dataset.reset);
      }));
  } catch (e) { handleErr(e); }
}
async function setKey(key, value) {
  try {
    const r = await call('config_set', { key, value });
    toast(r.message);
    await loadConfig(); await refreshOverview();
  } catch (e) { handleErr(e); await loadConfig(); }
}
async function resetKey(key) {
  if (!confirm('Reset ' + key + ' to its default value?')) return;
  try {
    const r = await call('config_reset', { key });
    toast(r.message); await loadConfig(); await refreshOverview();
  } catch (e) { handleErr(e); }
}

// ------------------------------------------------------------- language --
let _langReady = false;
function langParams(cmd, args) {
  return { cmd, args, file: document.getElementById('lang-file').value.trim(),
           cwd: document.getElementById('lang-cwd').value.trim() };
}
async function loadLanguage() {
  try {
    const s = await call('lang_info');
    document.getElementById('lang-info').innerHTML =
      '<div class="row"><span class="k">Toolchain</span><span class="v">' + esc(s.exe) + ' — ' +
      (s.found === 'true' ? '<span class="ok">' + esc(s.version.split('\n')[0]) + '</span>'
                          : '<span class="bad">not found — install spid3r.exe beside the panel</span>') +
      '</span></div>';
    if (!_langReady) {
      const sel = document.getElementById('lang-cmd');
      sel.innerHTML = s.commands.split(',').map(c => '<option>' + esc(c) + '</option>').join('');
      if (!document.getElementById('lang-cwd').value) document.getElementById('lang-cwd').value = s.cwd;
      _langReady = true;
    }
  } catch (e) { handleErr(e); }
}
async function langScan() {
  try {
    const r = await call('lang_files', { dir: document.getElementById('lang-cwd').value.trim() });
    const sel = document.getElementById('lang-pick');
    sel.innerHTML = '<option value="">— ' + (r.files ? r.files.split('\n').length : 0) + ' files —</option>' +
      (r.files ? r.files.split('\n').map(f => '<option value="' + esc(f) + '">' + esc(f) + '</option>').join('') : '');
  } catch (e) { handleErr(e); }
}
async function langExec(cmd, args, noFile) {
  const out = document.getElementById('lang-out');
  const st = document.getElementById('lang-status');
  const p = langParams(cmd, args);
  if (noFile) p.file = '';
  out.textContent = '$ spid3r ' + cmd + ' ' + args + ' …';
  st.style.display = 'none';
  try {
    const r = await call('lang_exec', p);
    out.textContent = '$ ' + r.command + '\n\n' + r.stdout + (r.stderr ? '\n[stderr]\n' + r.stderr : '');
    st.style.display = 'inline-block';
    st.textContent = 'exit ' + r.code + ' · ' + r.ms + ' ms';
    st.style.color = r.code === '0' ? 'var(--accent)' : 'var(--bad)';
    return r;
  } catch (e) { out.textContent = String(e); handleErr(e); }
}
function langRun() {
  const cmd = document.getElementById('lang-cmd').value;
  const globalOnly = [
    'version','help','info','init','install','tree','verify','pack','dist',
    'add','remove','search','publish','unpack','test'
  ].includes(cmd);
  const optionalFile = ['bench','magic','fmt','doc'].includes(cmd) && !document.getElementById('lang-file').value.trim();
  return langExec(cmd, document.getElementById('lang-args').value.trim(), globalOnly || optionalFile);
}
function langQuick(cmd, args, noFile) {
  const needsFile = !noFile && !document.getElementById('lang-file').value.trim();
  if (needsFile) { toast('Pick or type a source file first.', true); return; }
  if (cmd === 'run' && !args.includes('--sandbox') &&
      !confirm('Run this program WITHOUT the sandbox? It may use files, network and processes per your Safety settings.')) return;
  return langExec(cmd, args, noFile);
}
async function langTerm(cmd, noFile) {
  const p = langParams(cmd, '');
  if (noFile) p.file = '';
  else if (!p.file) { toast('Pick or type a source file first.', true); return; }
  try { const r = await call('lang_terminal', p); toast(r.message); } catch (e) { handleErr(e); }
}
async function langOpen() {
  const f = document.getElementById('lang-file').value.trim();
  if (!f) { toast('Type a file path first.', true); return; }
  const cwd = document.getElementById('lang-cwd').value.trim();
  try { const r = await call('lang_read', { path: f, cwd }); document.getElementById('lang-src').value = r.text; toast('opened ' + r.path); }
  catch (e) { handleErr(e); }
}
async function langSave() {
  const f = document.getElementById('lang-file').value.trim();
  if (!f) { toast('Type a file path first.', true); return false; }
  const cwd = document.getElementById('lang-cwd').value.trim();
  try { const r = await call('lang_write', { path: f, cwd, text: document.getElementById('lang-src').value }); toast(r.message); return true; }
  catch (e) { handleErr(e); return false; }
}
async function langSaveRun() { if (await langSave()) langExec('check', ''); }
document.getElementById('lang-pick').addEventListener('change', (ev) => {
  if (ev.target.value) { document.getElementById('lang-file').value = ev.target.value; langOpen(); }
});
document.getElementById('lang-src').addEventListener('keydown', (ev) => {
  if (ev.key === 'Tab') { ev.preventDefault(); const t = ev.target, s = t.selectionStart;
    t.value = t.value.slice(0, s) + '    ' + t.value.slice(t.selectionEnd); t.selectionStart = t.selectionEnd = s + 4; }
  if ((ev.ctrlKey || ev.metaKey) && ev.key === 's') { ev.preventDefault(); langSave(); }
});

// ----------------------------------------------------------------- logs --
async function loadAudit() {
  try {
    const s = await call('logs_tail', { n: 100 });
    document.getElementById('audit-text').textContent =
      (s.path ? '# ' + s.path + '\n' : '') + (s.text || '(empty)');
  } catch (e) { handleErr(e); }
}

document.querySelectorAll('nav button').forEach(b => b.addEventListener('click', () => {
  document.querySelectorAll('nav button').forEach(x => x.classList.toggle('on', x === b));
  document.querySelectorAll('main section').forEach(s => s.classList.add('hidden'));
  const tab = b.dataset.tab;
  document.getElementById('tab-' + tab).classList.remove('hidden');
  if (tab === 'overview') refreshOverview();
  if (tab === 'language') loadLanguage();
  if (tab === 'safety') loadSafety();
  if (tab === 'config') loadConfig();
  if (tab === 'audit') loadAudit();
}));
document.getElementById('cfg-filter').addEventListener('input', loadConfig);
refreshOverview();
</script>
</body>
</html>"##;

/// Best effort cleanup for stale ephemeral control-panel WebView2 folders.
#[cfg(target_os = "windows")]
fn prune_stale_control_profiles(control_root: &std::path::Path, current_pid: &str) {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(control_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy() == current_pid {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let stale = meta.is_dir()
            && meta
                .modified()
                .ok()
                .and_then(|mtime| now.duration_since(mtime).ok())
                .is_some_and(|age| age > MAX_AGE);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Open the control panel window and run its event loop until closed.
pub fn run() -> Result<(), String> {
    crate::config::schema::install_tz_validator(crate::stdlib::tz::check_timezone_setting);
    #[cfg(windows)]
    let event_loop = {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        let mut builder = EventLoopBuilder::<Msg>::with_user_event();
        builder.with_any_thread(true);
        builder.build()
    };
    #[cfg(not(windows))]
    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = tao::window::WindowBuilder::new()
        .with_title("Spid3r Control Panel")
        .with_inner_size(tao::dpi::LogicalSize::new(1080.0, 720.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(680.0, 500.0))
        .build(&event_loop)
        .map_err(|e| format!("cannot open the control panel window: {e}"))?;

    // Must use the registered `spid3r://` scheme on all platforms so wry's
    // custom protocol handler serves CONTROL_PAGE (same as `gui/window.rs`).
    let page_url = "spid3r://app/control.html";

    let mut builder = wry::WebViewBuilder::new()
        .with_initialization_script(INIT_SCRIPT)
        .with_ipc_handler({
            let proxy = proxy.clone();
            move |request: wry::http::Request<String>| {
                let _ = proxy.send_event(Msg::Op(request.into_body()));
            }
        })
        .with_custom_protocol("spid3r".to_string(), move |_id, _request| {
            HttpResponse::builder()
                .status(200)
                .header("Content-Type", "text/html; charset=utf-8")
                .body(Cow::Borrowed(CONTROL_PAGE.as_bytes()))
                .unwrap_or_else(|_| HttpResponse::new(Cow::Borrowed(&b""[..])))
        })
        .with_url(page_url);

    // Ephemeral webview profile: the panel keeps no localStorage state (all
    // state lives in spid3r.ini), and a per-PID folder lets multiple panels
    // run cleanly without locking one shared WebView2 profile directory.
    if cfg!(target_os = "windows") {
        #[cfg(target_os = "windows")]
        if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
            let root = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("Spid3r")
                .join("WebView2")
                .join("control");
            let pid = std::process::id().to_string();
            prune_stale_control_profiles(&root, &pid);
            let base = root.join(pid);
            let _ = std::fs::create_dir_all(&base);
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", base);
        }
        builder = builder.with_incognito(true);
    }

    let webview = builder.build(&window).map_err(|e| {
        format!("cannot start the webview (is the WebView2 runtime installed?): {e}")
    })?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            tao::event::Event::UserEvent(Msg::Op(body)) => {
                // Extract op/id/params from the page's flat JSON, dispatch,
                // and reply. A malformed message is dropped, never panicked.
                let obj = match parse_page_message(&body) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let (id, op, params) = obj;
                // Dispatch off the UI thread: toolchain actions can run for
                // seconds and must not freeze the window.
                let p = proxy.clone();
                crate::value::mark_multithreaded();
                std::thread::spawn(move || {
                    let reply = match ops::dispatch(&op, &params) {
                        Ok(data) => format!("{{\"ok\":true,\"data\":{}}}", data),
                        Err(e) => format!("{{\"ok\":false,\"error\":{}}}", ops::jstr(&e)),
                    };
                    let _ = p.send_event(Msg::Reply(format!(
                        "window.__spid3rReply({}, {});",
                        id, reply
                    )));
                });
            }
            tao::event::Event::UserEvent(Msg::Reply(js)) => {
                if let Err(e) = webview.evaluate_script(&js) {
                    eprintln!("warning[control-panel]: reply failed: {e}");
                }
            }
            _ => {}
        }
    });
}

/// Parse the page message `{op, id, params}` without a JSON dependency:
/// `id` and `op` are top-level scalars, `params` is captured as the raw
/// remainder so ops::dispatch's field() can read it.
fn parse_page_message(body: &str) -> Result<(u64, String, String), String> {
    let id = extract_number(body, "id").ok_or("missing id")?;
    let op = extract_string(body, "op").ok_or("missing op")?;
    let params = extract_object(body, "params").unwrap_or_else(|| "{}".to_string());
    Ok((id, op, params))
}

fn extract_number(json: &str, key: &str) -> Option<u64> {
    ops::field(json, key)?.parse().ok()
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    ops::field(json, key)
}

/// Grab the raw object text that follows a top-level `"key":` (balanced braces).
fn extract_object(json: &str, key: &str) -> Option<String> {
    let start = ops::raw_field_slice(json, key)?;
    if !start.starts_with('{') {
        return Some("{}".to_string());
    }
    let bytes = start.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_page_message_ignores_keys_inside_string_payloads() {
        let msg = r#"{"params":{"path":"a.matrix","text":"\"id\":999,\"params\":{\"x\":1}"},"id":42,"op":"lang_write"}"#;
        let (id, op, params) = parse_page_message(msg).unwrap();
        assert_eq!(id, 42);
        assert_eq!(op, "lang_write");
        assert_eq!(ops::field(&params, "path").as_deref(), Some("a.matrix"));
    }

    #[test]
    fn control_page_uses_custom_protocol_and_safe_commands() {
        assert!(CONTROL_PAGE.contains("handleErr(e)"));
        assert!(!CONTROL_PAGE.contains("runLocalOp"));
        assert!(!CONTROL_PAGE.contains("showAgent"));
    }
}

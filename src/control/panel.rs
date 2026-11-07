//! The `spid3r-control` desktop panel (feature `gui`).
//!
//! A tao/wry window over the same op layer the CLI uses ([`super::ops`]).
//! Deliberately NOT the `spid3r gui` program machinery: the panel is a fixed
//! native surface, so it never runs `.matrix` source, never touches the
//! plugin loader, and its IPC surface is a closed set of ops.
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
  <button data-tab="safety">Safety</button>
  <button data-tab="config">Configuration</button>
  <button data-tab="agent">Agent</button>
  <button data-tab="devices">Devices</button>
  <button data-tab="audit">Audit log</button>
  <div class="nav-danger">Every change on this panel is validated against the
    schema and written to the audit log.</div>
</nav>
<main>
  <section id="tab-overview">
    <h1>Overview</h1>
    <p class="sub">Runtime status for this machine.</p>
    <div class="card" id="overview-cards"></div>
  </section>
  <section id="tab-safety" class="hidden">
    <h1>Safety switches</h1>
    <p class="sub">What a Spid3r <b>program</b> may do on this machine. "Open"
       means programs can use the capability; "Restricted" means the runtime
       denies it. Changes take effect for newly started runs and are audited.</p>
    <div id="safety-banner"></div>
    <div id="safety-list"></div>
  </section>
  <section id="tab-config" class="hidden">
    <h1>Configuration</h1>
    <p class="sub" id="config-sub">spid3r.ini — grouped by section. Boolean and
       enum keys save immediately; text and number keys save on Set. Secret
       values are shown redacted.</p>
    <div style="margin-bottom:10px">
      <input type="text" id="cfg-filter" placeholder="filter keys…" style="width:260px">
      <button class="ghost" onclick="loadConfig()">Reload</button>
    </div>
    <div id="config-groups"></div>
  </section>
  <section id="tab-agent" class="hidden">
    <h1>Management agent</h1>
    <p class="sub">The local agent answers on [agent] bind:port from spid3r.ini.
       Remote hosts pair with <code>spid3r admin pair</code>; this panel only
       manages the agent on THIS machine.</p>
    <div class="card" id="agent-status"></div>
    <button class="act" onclick="agentStart()">Start agent</button>
    <button class="ghost" onclick="agentStop()">Stop agent</button>
    <button class="ghost" onclick="showAgent()">Refresh</button>
  </section>
  <section id="tab-devices" class="hidden">
    <h1>Paired devices</h1>
    <p class="sub">Devices allowed to call this machine's agent. Revoking a
       device takes effect immediately and is written to the audit log.</p>
    <div class="card" style="padding:0" id="dev-card">
      <div style="padding:12px 16px" id="dev-empty">…</div>
      <table id="dev-table" style="display:none">
        <thead><tr><th>ID</th><th>Name</th><th>Role</th><th>Last seen</th><th></th></tr></thead>
        <tbody></tbody>
      </table>
    </div>
  </section>
  <section id="tab-audit" class="hidden">
    <h1>Audit log</h1>
    <p class="sub">Panel and remote-agent actions. Enabled by
       <code>logging.audit</code> in spid3r.ini.</p>
    <button class="ghost" onclick="loadAudit()">Refresh</button>
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
  const s = await call('status');
  const ini = s.ini_exists === 'true'
    ? (s.ini_valid === 'true'
        ? '<span class="ok">valid</span>'
        : '<span class="bad">INVALID</span><br><code>' + esc(s.ini_errors) + '</code>')
    : '<span class="warn">no spid3r.ini yet</span> <button class="ghost" onclick="initIni()">write default</button>';
  const agent = s.agent_listening === 'true'
    ? '<span class="ok">listening</span> on ' + esc(s.agent_addr)
    : '<span class="bad">not listening</span> on ' + esc(s.agent_addr);
  document.getElementById('overview-cards').innerHTML =
    '<div class="row"><span class="k">Spid3r</span><span class="v">v' + esc(s.version) +
      ' (language ' + esc(s.language) + ')</span></div>' +
    '<div class="row"><span class="k">spid3r.ini</span><span class="v">' + esc(s.ini_path) + ' — ' + ini + '</span></div>' +
    '<div class="row"><span class="k">Agent</span><span class="v">' + agent + '</span></div>' +
    '<div class="row"><span class="k">Devices</span><span class="v">' + esc(s.devices) +
      ' paired · ' + esc(s.pairings) + ' pending</span></div>' +
    '<div class="row"><span class="k">Audit log</span><span class="v">' + esc(s.audit_lines) +
      ' lines · ' + esc(s.audit_path) + '</span></div>' +
    '<div class="row"><span class="k">State dir</span><span class="v">' + esc(s.state_dir) + '</span></div>';
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
}
async function toggleSafety(key, wantOpen) {
  const val = wantOpen ? openValue(key) : restrictValue(key);
  if (wantOpen && !confirm('Open ' + key + '?\n\nPrograms will be able to use this capability.')) {
    loadSafety(); return;
  }
  try {
    const r = await call('config_set', { key, value: val });
    toast(r.message);
    if (key === 'agent.enabled' && wantOpen) {
      toast('agent.enabled saved — start the listener in the Agent tab.');
    }
    await loadSafety(); await refreshOverview();
  } catch (e) { handleErr(e); await loadSafety(); }
}

// ---------------------------------------------------------------- config --
const SECTION_TITLES = {
  spid3r: 'Runtime', memory: 'Resource limits', debug: 'Debugging',
  logging: 'Logging', modules: 'Plugins', security: 'Security',
  agent: 'Management agent',
};
async function loadConfig() {
  const filter = document.getElementById('cfg-filter').value.toLowerCase();
  const s = await call('config_list');
  const groups = {};
  for (const row of s.keys.split('\n')) {
    if (!row) continue;
    const [key, kind, value, def, desc, options] = row.split('|');
    if (filter && !key.toLowerCase().includes(filter) &&
        !desc.toLowerCase().includes(filter)) continue;
    const sec = key.split('.')[0];
    (groups[sec] = groups[sec] || []).push({ key, kind, value, def, desc, options });
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
}
async function setKey(key, value) {
  if (value === '') return;
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

// ---------------------------------------------------------------- agent --
async function agentStart() {
  try { const r = await call('agent_start'); toast(r.message); await showAgent(); await refreshOverview(); }
  catch (e) { handleErr(e); }
}
async function agentStop() {
  try { const r = await call('agent_stop'); toast(r.message); await showAgent(); await refreshOverview(); }
  catch (e) { handleErr(e); }
}
async function showAgent() {
  const s = await call('agent_status');
  document.getElementById('agent-status').innerHTML =
    '<div class="row"><span class="k">Status</span><span class="v">' +
    (s.listening === 'true' ? '<span class="ok">listening</span>' : '<span class="bad">stopped</span>') +
    '</span></div>' +
    '<div class="row"><span class="k">Address</span><span class="v">' + esc(s.addr) + '</span></div>' +
    '<div class="row"><span class="k">Tracked PID</span><span class="v">' + (esc(s.pid) || '—') + '</span></div>';
}

// -------------------------------------------------------------- devices --
async function loadDevices() {
  const s = await call('devices_list');
  const empty = document.getElementById('dev-empty');
  const table = document.getElementById('dev-table');
  if (!s.devices) {
    table.style.display = 'none';
    empty.style.display = 'block';
    empty.innerHTML = 'no devices paired — pair one with <code>spid3r agent pair</code>';
    return;
  }
  empty.style.display = 'none';
  table.style.display = 'table';
  const tb = table.querySelector('tbody'); tb.innerHTML = '';
  for (const row of s.devices.split('\n')) {
    if (!row) continue;
    const [id, name, role, seen] = row.split('|');
    const tr = document.createElement('tr');
    tr.innerHTML = '<td><code>' + esc(id) + '</code></td><td>' + esc(name) + '</td><td>' + esc(role) +
      '</td><td>' + esc(seen) + '</td>' +
      '<td><button class="ghost" data-revoke="' + esc(id) + '">Revoke</button></td>';
    tb.appendChild(tr);
  }
}

// ---------------------------------------------------------------- audit --
async function loadAudit() {
  const s = await call('audit_tail', { n: 100 });
  document.getElementById('audit-text').textContent = s.text || '(empty — nothing audited yet)';
}

// Delegated listeners: rows are rebuilt on every refresh, so handlers live on
// stable parents and read the target from data attributes. No untrusted text
// is ever interpolated into an inline handler (security review F-2).
document.getElementById('dev-table').addEventListener('click', (ev) => {
  const btn = ev.target.closest('[data-revoke]');
  if (!btn) return;
  if (confirm('Revoke device ' + btn.dataset.revoke + '? It loses agent access immediately.')) {
    call('devices_revoke', { id: btn.dataset.revoke })
      .then(r => { toast(r.message); return loadDevices().then(refreshOverview); })
      .catch(handleErr);
  }
});
document.querySelectorAll('nav button').forEach(b => b.addEventListener('click', () => {
  document.querySelectorAll('nav button').forEach(x => x.classList.toggle('on', x === b));
  document.querySelectorAll('main section').forEach(s => s.classList.add('hidden'));
  const tab = b.dataset.tab;
  document.getElementById('tab-' + tab).classList.remove('hidden');
  if (tab === 'overview') refreshOverview();
  if (tab === 'safety') loadSafety();
  if (tab === 'config') loadConfig();
  if (tab === 'agent') showAgent();
  if (tab === 'devices') loadDevices();
  if (tab === 'audit') loadAudit();
}));
document.getElementById('cfg-filter').addEventListener('input', loadConfig);
refreshOverview();
</script>
</body>
</html>"##;

/// Open the control panel window and run its event loop until closed.
pub fn run() -> Result<(), String> {
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

    let page_url = if cfg!(target_os = "windows") {
        // wry's IPC handler unwraps its URL parse; `http://spid3r.app/...` is
        // the Windows-safe form of the custom origin (see gui/window.rs B5).
        "http://spid3r.app/control.html"
    } else {
        "spid3r://app/control.html"
    };

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
    // state lives in spid3r.ini / the agent store), and a per-PID folder lets
    // two panels run at once.
    if cfg!(target_os = "windows") {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Spid3r")
            .join("WebView2")
            .join("control")
            .join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(&base);
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", base);
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
                let reply = match ops::dispatch(&op, &params) {
                    Ok(data) => format!("{{\"ok\":true,\"data\":{}}}", data),
                    Err(e) => format!("{{\"ok\":false,\"error\":{}}}", ops::jstr(&e)),
                };
                let js = format!("window.__spid3rReply({}, {});", id, reply);
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
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let after = json[pos + needle.len()..].trim_start().strip_prefix(':')?;
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    // Reuse the ops field scanner by wrapping the body in a fake object whose
    // keys are the same shape ({"op":"..."}).
    ops::field(json, key)
}

/// Grab the raw object/value text that follows `"key":` (balanced braces).
fn extract_object(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let pos = json.find(&needle)?;
    let after = json[pos + needle.len()..].trim_start().strip_prefix(':')?;
    let start = after.trim_start();
    let open = start.chars().next()?;
    if open != '{' {
        // Not an object (e.g. `{}`): return it verbatim as an empty object.
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

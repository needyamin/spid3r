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
  :root { --bg:#f6f8f7; --panel:#ffffff; --ink:#1c2422; --muted:#66736e;
          --line:#dfe6e2; --accent:#0f8a5f; --accent-soft:#e3f3ec;
          --warn:#b45309; --bad:#b91c1c; }
  * { box-sizing: border-box; }
  body { margin:0; font:14px/1.5 "Segoe UI",system-ui,sans-serif;
         background:var(--bg); color:var(--ink); display:flex; height:100vh; }
  nav { width:190px; background:var(--panel); border-right:1px solid var(--line);
        padding:18px 0; flex-shrink:0; }
  .brand { font-weight:700; font-size:16px; padding:0 20px 14px; }
  .brand small { display:block; color:var(--muted); font-weight:400; font-size:11px; }
  nav button { display:block; width:100%; text-align:left; padding:10px 20px;
        border:0; background:none; cursor:pointer; font-size:14px; color:var(--ink); }
  nav button:hover { background:var(--accent-soft); }
  nav button.on { background:var(--accent-soft); color:var(--accent);
        font-weight:600; border-right:3px solid var(--accent); }
  main { flex:1; overflow:auto; padding:26px 32px; }
  h1 { font-size:20px; margin:0 0 4px; }
  .sub { color:var(--muted); margin:0 0 22px; font-size:13px; }
  .card { background:var(--panel); border:1px solid var(--line); border-radius:10px;
          padding:16px 18px; margin-bottom:14px; }
  .row { display:flex; justify-content:space-between; gap:16px; padding:5px 0; }
  .row .k { color:var(--muted); }
  .row .v { text-align:right; word-break:break-all; }
  .ok { color:var(--accent); font-weight:600; }
  .bad { color:var(--bad); font-weight:600; }
  .warn { color:var(--warn); font-weight:600; }
  table { width:100%; border-collapse:collapse; font-size:13px; }
  th,td { text-align:left; padding:6px 8px; border-bottom:1px solid var(--line); }
  th { color:var(--muted); font-weight:600; }
  td code { font-size:12px; }
  input[type=text], input[type=number] { padding:6px 8px; border:1px solid var(--line);
        border-radius:6px; font:13px/1.4 monospace; width:100%; }
  button.act { background:var(--accent); color:#fff; border:0; border-radius:6px;
        padding:7px 14px; cursor:pointer; font-size:13px; }
  button.act:hover { filter:brightness(1.08); }
  button.ghost { background:none; border:1px solid var(--line); color:var(--ink);
        border-radius:6px; padding:6px 12px; cursor:pointer; font-size:13px; }
  pre { background:#f0f3f1; border:1px solid var(--line); border-radius:8px;
        padding:12px; font:12px/1.5 Consolas,monospace; overflow:auto;
        max-height:340px; white-space:pre-wrap; }
  .hidden { display:none; }
  #toast { position:fixed; bottom:18px; right:18px; background:var(--ink); color:#fff;
        padding:10px 16px; border-radius:8px; font-size:13px; opacity:0;
        transition:opacity .25s; pointer-events:none; max-width:420px; }
  #toast.show { opacity:.96; }
</style>
</head>
<body>
<nav>
  <div class="brand">Spid3r<small>Control Panel</small></div>
  <button data-tab="overview" class="on">Overview</button>
  <button data-tab="config">Configuration</button>
  <button data-tab="agent">Agent</button>
  <button data-tab="devices">Devices</button>
  <button data-tab="audit">Audit log</button>
</nav>
<main>
  <section id="tab-overview">
    <h1>Overview</h1>
    <p class="sub">Runtime status for this machine.</p>
    <div class="card" id="overview-cards"></div>
  </section>
  <section id="tab-config" class="hidden">
    <h1>Configuration</h1>
    <p class="sub">spid3r.ini — every known key, validated on save. Secret values
       are shown redacted; type the new value to change one.</p>
    <div style="margin-bottom:10px">
      <input type="text" id="cfg-filter" placeholder="filter keys…" style="width:280px">
      <button class="ghost" onclick="loadConfig()">Reload</button>
    </div>
    <div class="card" style="padding:0">
      <table id="cfg-table">
        <thead><tr><th>Key</th><th>Type</th><th>Value</th><th style="width:230px">Change</th></tr></thead>
        <tbody></tbody>
      </table>
    </div>
  </section>
  <section id="tab-agent" class="hidden">
    <h1>Management agent</h1>
    <p class="sub">The local agent answers on [agent] bind:port from spid3r.ini.
       Remote hosts pair with <code>spid3r admin pair</code>; this panel only
       manages the agent on THIS machine.</p>
    <div class="card" id="agent-status"></div>
    <button class="act" onclick="call('agent_start').then(showAgent).then(refreshOverview).catch(toast)">Start agent</button>
    <button class="ghost" onclick="call('agent_stop').then(showAgent).then(refreshOverview).catch(toast)">Stop agent</button>
    <button class="ghost" onclick="showAgent()">Refresh</button>
  </section>
  <section id="tab-devices" class="hidden">
    <h1>Paired devices</h1>
    <p class="sub">Devices allowed to call this machine's agent. Revoking a
       device takes effect immediately and is written to the audit log.</p>
    <div class="card" style="padding:0">
      <table id="dev-table">
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
function toast(msg) { const t = document.getElementById('toast');
  t.textContent = msg; t.classList.add('show');
  clearTimeout(t._h); t._h = setTimeout(() => t.classList.remove('show'), 4200); }
function esc(s) { const d = document.createElement('div'); d.textContent = s ?? ''; return d.innerHTML; }
function badge(b, good, badText, goodText) {
  return b ? '<span class="ok">' + (goodText || 'yes') + '</span>'
           : '<span class="bad">' + (badText || 'no') + '</span>';
}
async function refreshOverview() {
  const s = await call('status');
  const ini = s.ini_exists === 'true'
    ? (s.ini_valid === 'true'
        ? '<span class="ok">valid</span>'
        : '<span class="bad">INVALID</span><br><code>' + esc(s.ini_errors) + '</code>')
    : '<span class="warn">no spid3r.ini yet</span> <button class="ghost" onclick="initIni()">write default</button>';
  const agent = s.agent_listening === 'true'
    ? '<span class="ok">listening</span> on ' + esc(s.agent_addr)
    : '<span class="bad">not listening</span> on ' + esc(s.agent_addr) +
      ' <button class="ghost" onclick="call(\'agent_start\').then(showAgent).then(refreshOverview).catch(toast)">start</button>';
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
async function loadConfig() {
  const filter = document.getElementById('cfg-filter').value.toLowerCase();
  const s = await call('config_list');
  const tb = document.querySelector('#cfg-table tbody'); tb.innerHTML = '';
  for (const row of s.keys.split('\n')) {
    if (!row) continue;
    const [key, kind, value, def] = row.split('|');
    if (filter && !key.toLowerCase().includes(filter)) continue;
    const tr = document.createElement('tr');
    tr.innerHTML = '<td><code>' + esc(key) + '</code></td><td>' + esc(kind) +
      '</td><td><code>' + esc(value) + '</code>' +
      (value !== def ? ' <span style="color:var(--muted)">(default: ' + esc(def) + ')</span>' : '') +
      '</td><td><input type="text" data-key-input="' + esc(key) + '" placeholder="new value">' +
      '<button class="act" style="margin-top:4px" data-set-key="' + esc(key) + '">Set</button></td>';
    tb.appendChild(tr);
  }
}
async function setKey(key) {
  const el = document.querySelector('[data-key-input="' + CSS.escape(key) + '"]');
  if (!el || el.value === '') return;
  try {
    const r = await call('config_set', { key, value: el.value });
    toast(r.message); await loadConfig(); await refreshOverview();
  } catch (e) { toast('rejected: ' + e); }
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
async function loadDevices() {
  const s = await call('devices_list');
  const tb = document.querySelector('#dev-table tbody'); tb.innerHTML = '';
  if (!s.devices) {
    tb.innerHTML = '<tr><td colspan="5" style="color:var(--muted)">no devices paired — pair one with <code>spid3r agent pair</code></td></tr>';
    return;
  }
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
async function loadAudit() {
  const s = await call('audit_tail', { n: 100 });
  document.getElementById('audit-text').textContent = s.text || '(empty — nothing audited yet)';
}
async function initIni() {
  try { const r = await call('ini_init', { force: false }); toast(r.message); await refreshOverview(); }
  catch (e) { toast('rejected: ' + e); }
}
// Delegated event listeners: rows are rebuilt on every refresh, so handlers
// live on the table and read the target from data attributes. No untrusted
// text is ever interpolated into an inline handler (security review F-2).
document.getElementById('cfg-table').addEventListener('click', (ev) => {
  const btn = ev.target.closest('[data-set-key]');
  if (btn) setKey(btn.dataset.setKey);
});
document.getElementById('dev-table').addEventListener('click', (ev) => {
  const btn = ev.target.closest('[data-revoke]');
  if (btn) {
    if (confirm('Revoke device ' + btn.dataset.revoke + '? It loses agent access immediately.')) {
      call('devices_revoke', { id: btn.dataset.revoke })
        .then(r => { toast(r.message); return loadDevices().then(refreshOverview); })
        .catch(e => toast('rejected: ' + e));
    }
  }
});
document.querySelectorAll('nav button').forEach(b => b.addEventListener('click', () => {
  document.querySelectorAll('nav button').forEach(x => x.classList.toggle('on', x === b));
  document.querySelectorAll('main section').forEach(s => s.classList.add('hidden'));
  const tab = b.dataset.tab;
  document.getElementById('tab-' + tab).classList.remove('hidden');
  if (tab === 'overview') refreshOverview();
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
        .with_inner_size(tao::dpi::LogicalSize::new(1020.0, 700.0))
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

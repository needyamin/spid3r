//! The window half of the GUI layer: a `tao` window with a `wry` webview,
//! plus the headless `--dry-run` replay the tests assert against.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wry::http::{Request as HttpRequest, Response as HttpResponse};

use crate::diagnostics::Span;
use crate::interpreter::Flow;
use crate::stdlib::shutdown;
use crate::value::Value;

use super::{bridge, Runtime};

/// The page shown when no `--html` page is given. `#spid3r-root` is the
/// element `gui_set_html` replaces.
pub const DEFAULT_PAGE: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>spid3r gui</title></head>
<body style="font:14px/1.5 system-ui,sans-serif;margin:2rem">
  <div id="spid3r-root">
    <p>The spid3r GUI window is running. Build the UI from your program with
    <code>gui_set_html(...)</code> / <code>gui_eval(...)</code>,
    and react to the page with <code>gui_on(event, handler)</code>.</p>
    <p>or load an existing page with <code>spid3r gui app.matrix --html page.html</code>.</p>
  </div>
</body>
</html>"#;

/// Injected before any page script. `window.spid3r.emit(event, payload)`
/// posts the event to the Rust side, where it is dispatched to the program's
/// `gui_on` handler as `handler(event, payload)`.
const INIT_SCRIPT: &str = r#"window.spid3r = {
  emit: function (event, payload) {
    try {
      window.ipc.postMessage(JSON.stringify({ event: event, payload: payload }));
    } catch (e) {}
  }
};"#;

/// Wakes the window's event loop when the page, a worker thread, or shutdown notifies it.
pub enum Msg {
    Event,
    PageReady,
    Shutdown,
}

/// Initial window and webview configuration (populated from CLI flags and
/// Phase-1 `gui_*` calls before the window opens).
#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub width: f64,
    pub height: f64,
    pub min_size: Option<(f64, f64)>,
    pub maximized: bool,
    pub fullscreen: bool,
    pub resizable: bool,
    pub decorations: bool,
    pub theme: Option<bridge::WindowTheme>,
    pub bg_color: Option<(u8, u8, u8, u8)>,
    pub icon: Option<String>,
    pub devtools: bool,
    pub clipboard: bool,
    pub zoom: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            width: 920.0,
            height: 640.0,
            min_size: Some((320.0, 200.0)),
            maximized: false,
            fullscreen: false,
            resizable: true,
            decorations: true,
            theme: None,
            bg_color: None,
            icon: None,
            devtools: cfg!(debug_assertions),
            clipboard: true,
            zoom: false,
        }
    }
}

impl WindowOptions {
    /// Fold Phase-1 `WindowCommand`s into initial window options.
    /// Returns `true` if `WindowCommand::Close` was requested during Phase 1.
    pub fn apply_phase1_cmds(&mut self, cmds: Vec<bridge::WindowCommand>) -> bool {
        let mut close_requested = false;
        for cmd in cmds {
            match cmd {
                bridge::WindowCommand::Icon(p) => self.icon = Some(p),
                bridge::WindowCommand::Size(w, h) => {
                    self.width = w;
                    self.height = h;
                }
                bridge::WindowCommand::MinSize(w, h) => self.min_size = Some((w, h)),
                bridge::WindowCommand::Maximized(m) => self.maximized = m,
                bridge::WindowCommand::Fullscreen(f) => self.fullscreen = f,
                bridge::WindowCommand::Resizable(r) => self.resizable = r,
                bridge::WindowCommand::Decorations(d) => self.decorations = d,
                bridge::WindowCommand::Theme(t) => self.theme = Some(t),
                bridge::WindowCommand::BgColor(r, g, b, a) => self.bg_color = Some((r, g, b, a)),
                bridge::WindowCommand::DevTools(d) => self.devtools = d,
                bridge::WindowCommand::Close => close_requested = true,
            }
        }
        close_requested
    }
}

/// Replay the bridge without a window: the `ready` event is dispatched and
/// the queued JavaScript is printed (`js: …`) instead of evaluated.
pub fn dry_run(runtime: &Runtime) {
    bridge::push_event("ready", "");
    dispatch(runtime);
    for js in bridge::take_out() {
        println!("js: {}", js);
    }
    // Wait for background `spawn` tasks after `ready` runs (so handlers that call
    // `request_shutdown()` stop cooperative worker loops cleanly) and drain any
    // JS or events those workers pushed.
    crate::sched::join_tasks_at_exit();
    dispatch(runtime);
    for js in bridge::take_out() {
        println!("js: {}", js);
    }
}

/// Where the webview's page and its assets come from.
pub enum PageSource {
    /// The built-in placeholder page, no filesystem access.
    Builtin,
    /// `--html page.html`: served from disk, assets resolved beside it.
    File(String),
    /// `--payload app.exe`: served straight out of the executable's embedded
    /// payload, so nothing is unpacked to disk.
    Memory {
        entry: String,
        files: Arc<std::collections::HashMap<String, Vec<u8>>>,
    },
}

/// SEC-M2 (audit fix): strict origin-boundary allowlist for webview navigation.
/// Only the app's own custom-protocol origin (`spid3r://app/...`, plus
/// `https://spid3r.app/...` on Windows where `with_https_scheme(true)` rewrites
/// the custom protocol to an HTTPS origin) and `about:blank` are permitted.
/// On non-Windows platforms `with_https_scheme(true)` is not active, so
/// `https://spid3r.app` is NOT allowlisted there (preventing navigation to a
/// live internet domain). Lookalike hosts such as `https://spid3r.app.evil.com/`
/// or `https://spid3r.app@evil.com/` are rejected because the prefix must end
/// at EOF, `/`, `?`, or `#`.
pub(crate) fn is_allowed_webview_url(url: &str) -> bool {
    is_allowed_webview_url_with_https(url, cfg!(target_os = "windows"))
}

fn is_allowed_webview_url_with_https(url: &str, allow_https_app: bool) -> bool {
    if url == "about:blank" {
        return true;
    }
    let matches_origin = |origin: &str| {
        url.strip_prefix(origin).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with('/')
                || rest.starts_with('?')
                || rest.starts_with('#')
        })
    };
    if matches_origin("spid3r://app") {
        return true;
    }
    if allow_https_app && matches_origin("https://spid3r.app") {
        return true;
    }
    false
}

/// Open the GUI window and run its event loop until the window closes.
/// Returns only if the window could not be created at all.
///
/// `ephemeral` runs the webview on an in-memory profile: no HTTP cache, no
/// code cache, no history. Embedded payload assets would otherwise be copied
/// into the user profile, which defeats shipping an app as one opaque file.
/// The cost is that the page's `localStorage` does not survive a restart —
/// programs that need durable state should keep using the bridge.
///
/// `app_id` keys the on-disk profile (see [`select_webview_profile`]); it is
/// ignored on platforms other than Windows.
pub fn open(
    runtime: Runtime,
    source: PageSource,
    ephemeral: bool,
    app_id: &str,
    mut opts: WindowOptions,
) -> Result<(), String> {
    if opts.apply_phase1_cmds(bridge::take_window_cmds()) || shutdown::is_requested() {
        bridge::mark_closed();
        crate::sched::join_tasks_at_exit();
        return Ok(());
    }
    let title = bridge::take_title().unwrap_or_else(|| "spid3r gui".to_string());

    // `cli::run` keeps `spid3r gui` on the OS main thread (required by macOS
    // AppKit and Linux GTK), while Windows/Unix embedders may still call
    // `open()` from a worker thread when `with_any_thread(true)` is available.
    #[cfg(windows)]
    let event_loop = {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        let mut builder = tao::event_loop::EventLoopBuilder::<Msg>::with_user_event();
        builder.with_any_thread(true);
        builder.build()
    };
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    let event_loop = {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        let mut builder = tao::event_loop::EventLoopBuilder::<Msg>::with_user_event();
        builder.with_any_thread(true);
        builder.build()
    };
    #[cfg(not(any(
        windows,
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    let event_loop = tao::event_loop::EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[cfg(target_os = "windows")]
    select_webview_profile(ephemeral, app_id);
    #[cfg(not(target_os = "windows"))]
    let _ = app_id;

    let site = Arc::new(match source {
        PageSource::File(path) => StaticSite::from_html_path(&path)?,
        PageSource::Memory { entry, files } => StaticSite::from_memory(entry, files),
        PageSource::Builtin => StaticSite::builtin(),
    });

    let initial_icon = resolve_window_icon(opts.icon.as_deref(), &site);

    let mut wb = tao::window::WindowBuilder::new()
        .with_title(title)
        .with_inner_size(tao::dpi::LogicalSize::new(opts.width, opts.height))
        .with_maximized(opts.maximized)
        .with_resizable(opts.resizable)
        .with_decorations(opts.decorations);
    if let Some((mw, mh)) = opts.min_size {
        wb = wb.with_min_inner_size(tao::dpi::LogicalSize::new(mw, mh));
    }
    if opts.fullscreen {
        wb = wb.with_fullscreen(Some(tao::window::Fullscreen::Borderless(None)));
    }
    if let Some(t) = opts.theme.and_then(to_tao_theme) {
        wb = wb.with_theme(Some(t));
    }
    if let Some(ic) = initial_icon {
        #[cfg(target_os = "windows")]
        {
            use tao::platform::windows::WindowBuilderExtWindows;
            wb = wb
                .with_window_icon(Some(ic.clone()))
                .with_taskbar_icon(Some(ic));
        }
        #[cfg(not(target_os = "windows"))]
        {
            wb = wb.with_window_icon(Some(ic));
        }
    }
    let window = wb
        .build(&event_loop)
        .map_err(|e| format!("cannot open the GUI window: {e}"))?;

    let mut builder = wry::WebViewBuilder::new()
        .with_initialization_script(INIT_SCRIPT)
        .with_incognito(ephemeral)
        .with_devtools(opts.devtools)
        .with_clipboard(opts.clipboard)
        .with_hotkeys_zoom(opts.zoom)
        // SEC-M2 (audit fix): pin navigation strictly to the app's own
        // custom-protocol origin so `window.spid3r.emit` can never follow a
        // navigation onto a remote or lookalike origin (e.g.
        // `https://spid3r.app.evil.com`).
        .with_navigation_handler(|url| is_allowed_webview_url(&url))
        .with_ipc_handler({
            let proxy = proxy.clone();
            move |request: wry::http::Request<String>| {
                let body = request.into_body();
                match bridge::decode_ipc_message(&body) {
                    Ok((event, payload)) => {
                        bridge::push_event(&event, &payload);
                        // The loop is waiting; nudge it to dispatch.
                        let _ = proxy.send_event(Msg::Event);
                    }
                    Err(err) => {
                        eprintln!("error[runtime]: {}", err);
                    }
                }
            }
        })
        .with_drag_drop_handler({
            let proxy = proxy.clone();
            move |ev: wry::DragDropEvent| {
                if let wry::DragDropEvent::Drop { paths, position } = ev {
                    let event_name = if bridge::has_handler("file_drop") {
                        Some("file_drop")
                    } else if bridge::has_handler("drop") {
                        Some("drop")
                    } else {
                        None
                    };
                    if let Some(name) = event_name {
                        let json = encode_file_drop_json(&paths, position);
                        bridge::push_event(name, &json);
                        let _ = proxy.send_event(Msg::Event);
                        return true;
                    }
                }
                false
            }
        })
        .with_on_page_load_handler({
            let proxy = proxy.clone();
            move |event: wry::PageLoadEvent, _url: String| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    let _ = proxy.send_event(Msg::PageReady);
                }
            }
        });
    if let Some(rgba) = opts.bg_color {
        builder = builder.with_background_color(rgba);
    }
    #[cfg(target_os = "windows")]
    {
        use wry::WebViewBuilderExtWindows;
        builder = builder.with_https_scheme(true);
    }

    // The page is served through the `spid3r://app/` custom protocol instead
    // of `file://` (B5): wry's IPC handler builds an `http::Request` whose URI
    // is the page's own URL and `unwrap()`s the parse, and the `http` crate
    // rejects any absolute URI without an authority — `file:///C:/...` and
    // `about:blank` both fail. With `with_https_scheme(true)` on Windows the
    // origin is `https://spid3r.app`, giving a secure context (`isSecureContext`,
    // `crypto.subtle`, `navigator.clipboard`).
    builder = builder.with_custom_protocol("spid3r".to_string(), {
        let site = Arc::clone(&site);
        move |_id, request| site.serve(request)
    });
    builder = builder.with_url(site.page_url());

    let webview = builder.build(&window).map_err(|e| {
        format!("cannot start the webview (is the WebView2 runtime installed?): {e}")
    })?;

    let waker_proxy = proxy.clone();
    bridge::set_waker(Some(Arc::new(move || {
        let _ = waker_proxy.send_event(Msg::Event);
    })));
    let shutdown_proxy = proxy.clone();
    shutdown::set_hook(Some(Arc::new(move || {
        let _ = shutdown_proxy.send_event(Msg::Shutdown);
    })));

    // Gate JS evaluation behind `page_ready` (set on `PageLoadEvent::Finished`)
    // so Phase-1 `gui_set_html` / `gui_eval` and early `spawn` worker calls
    // never execute before the DOM and `#spid3r-root` finish loading.
    let mut page_ready = false;
    let mut initial_ready_sent = false;
    let site_for_loop = Arc::clone(&site);

    // The loop owns the engine handle and the webview from here on.
    event_loop.run(move |event, _, control_flow| {
        *control_flow = tao::event_loop::ControlFlow::Wait;
        match event {
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } => {
                let mut cancelled = false;
                let mut forced_close = false;
                if let Some(entry) = bridge::get_handler_entry("before_close") {
                    bridge::reset_close_cancelled();
                    dispatch_one(&runtime, "before_close", "{}", entry);
                    cancelled = bridge::take_close_cancelled();
                    if apply_live_cmds(&window, &webview, &site_for_loop) {
                        forced_close = true;
                    }
                    if page_ready {
                        flush(&webview);
                    }
                }
                if !cancelled && !forced_close {
                    if let Some(entry) = bridge::get_handler_entry("close") {
                        bridge::reset_close_cancelled();
                        dispatch_one(&runtime, "close", "{}", entry);
                        cancelled = bridge::take_close_cancelled();
                        if apply_live_cmds(&window, &webview, &site_for_loop) {
                            forced_close = true;
                        }
                        if page_ready {
                            flush(&webview);
                        }
                    }
                }
                if !cancelled || forced_close {
                    shutdown::set_hook(None);
                    shutdown::request();
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                }
            }
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::Resized(size),
                ..
            } => {
                if bridge::has_handler("resize") {
                    let payload =
                        format!("{{\"width\":{},\"height\":{}}}", size.width, size.height);
                    bridge::push_event("resize", &payload);
                    let _ = proxy.send_event(Msg::Event);
                }
            }
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::Focused(focused),
                ..
            } => {
                if bridge::has_handler("focus") {
                    let payload = format!("{{\"focused\":{}}}", focused);
                    bridge::push_event("focus", &payload);
                    let _ = proxy.send_event(Msg::Event);
                }
            }
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::ThemeChanged(theme),
                ..
            } => {
                if bridge::has_handler("theme_changed") {
                    let label = match theme {
                        tao::window::Theme::Dark => "dark",
                        _ => "light",
                    };
                    let payload = format!("{{\"theme\":\"{}\"}}", label);
                    bridge::push_event("theme_changed", &payload);
                    let _ = proxy.send_event(Msg::Event);
                }
            }
            tao::event::Event::UserEvent(Msg::Shutdown) => {
                shutdown::set_hook(None);
                *control_flow = tao::event_loop::ControlFlow::Exit;
            }
            tao::event::Event::UserEvent(Msg::PageReady) => {
                page_ready = true;
                if !initial_ready_sent {
                    initial_ready_sent = true;
                    bridge::push_event("ready", "");
                }
                if apply_live_cmds(&window, &webview, &site_for_loop) {
                    shutdown::set_hook(None);
                    shutdown::request();
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                    return;
                }
                dispatch(&runtime);
                if apply_live_cmds(&window, &webview, &site_for_loop) {
                    shutdown::set_hook(None);
                    shutdown::request();
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                    return;
                }
                flush(&webview);
                if shutdown::is_requested() {
                    shutdown::set_hook(None);
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                }
            }
            tao::event::Event::UserEvent(Msg::Event) => {
                if apply_live_cmds(&window, &webview, &site_for_loop) {
                    shutdown::set_hook(None);
                    shutdown::request();
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                    return;
                }
                dispatch(&runtime);
                if apply_live_cmds(&window, &webview, &site_for_loop) {
                    shutdown::set_hook(None);
                    shutdown::request();
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                    return;
                }
                if page_ready {
                    flush(&webview);
                }
                if shutdown::is_requested() {
                    shutdown::set_hook(None);
                    *control_flow = tao::event_loop::ControlFlow::Exit;
                }
            }
            tao::event::Event::LoopDestroyed => {
                shutdown::set_hook(None);
                shutdown::request();
                bridge::mark_closed();
                crate::sched::join_tasks_at_exit();
            }
            _ => {}
        }
    });
}

fn to_tao_theme(t: bridge::WindowTheme) -> Option<tao::window::Theme> {
    match t {
        bridge::WindowTheme::Light => Some(tao::window::Theme::Light),
        bridge::WindowTheme::Dark => Some(tao::window::Theme::Dark),
        bridge::WindowTheme::System => None,
    }
}

fn resolve_window_icon(spec: Option<&str>, site: &StaticSite) -> Option<tao::window::Icon> {
    if let Some(path) = spec {
        let bytes = site
            .read_asset(path)
            .or_else(|| crate::stdlib::read_file_bytes_capped(path, "gui_icon").ok());
        match bytes {
            Some(b) => match super::icon::load_tao_icon(&b) {
                Ok(ic) => return Some(ic),
                Err(e) => eprintln!("error[runtime]: cannot decode icon '{}': {}", path, e),
            },
            None => eprintln!("error[runtime]: cannot read icon '{}'", path),
        }
    }
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::IconExtWindows;
        if let Ok(ic) = tao::window::Icon::from_resource(1, None) {
            return Some(ic);
        }
    }
    None
}

fn apply_live_cmds(
    window: &tao::window::Window,
    webview: &wry::WebView,
    site: &StaticSite,
) -> bool {
    if let Some(new_title) = bridge::take_title() {
        window.set_title(&new_title);
    }
    let mut close = false;
    for cmd in bridge::take_window_cmds() {
        match cmd {
            bridge::WindowCommand::Icon(path) => {
                if let Some(ic) = resolve_window_icon(Some(&path), site) {
                    #[cfg(target_os = "windows")]
                    {
                        use tao::platform::windows::WindowExtWindows;
                        window.set_window_icon(Some(ic.clone()));
                        window.set_taskbar_icon(Some(ic));
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        window.set_window_icon(Some(ic));
                    }
                }
            }
            bridge::WindowCommand::Size(w, h) => {
                window.set_inner_size(tao::dpi::LogicalSize::new(w, h));
            }
            bridge::WindowCommand::MinSize(w, h) => {
                window.set_min_inner_size(Some(tao::dpi::LogicalSize::new(w, h)));
            }
            bridge::WindowCommand::Maximized(m) => {
                window.set_maximized(m);
            }
            bridge::WindowCommand::Fullscreen(f) => {
                window.set_fullscreen(if f {
                    Some(tao::window::Fullscreen::Borderless(None))
                } else {
                    None
                });
            }
            bridge::WindowCommand::Resizable(r) => {
                window.set_resizable(r);
            }
            bridge::WindowCommand::Decorations(d) => {
                window.set_decorations(d);
            }
            bridge::WindowCommand::Theme(t) => {
                window.set_theme(to_tao_theme(t));
            }
            bridge::WindowCommand::BgColor(r, g, b, a) => {
                let _ = webview.set_background_color((r, g, b, a));
            }
            bridge::WindowCommand::DevTools(open) => {
                if open {
                    webview.open_devtools();
                } else {
                    webview.close_devtools();
                }
            }
            bridge::WindowCommand::Close => {
                close = true;
            }
        }
    }
    close
}

fn encode_file_drop_json(paths: &[PathBuf], pos: (i32, i32)) -> String {
    let mut out = String::from("{\"paths\":[");
    for (idx, p) in paths.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&bridge::json_quote(&p.to_string_lossy()));
    }
    out.push_str(&format!("],\"x\":{},\"y\":{}}}", pos.0, pos.1));
    out
}

/// Point wry's WebView2 at a profile folder that belongs to *this* app.
///
/// `wry` reads `WEBVIEW2_USER_DATA_FOLDER` when it creates the webview
/// environment, so the folder is chosen by exporting that variable before the
/// window is built. A variable the host already set wins — that is how an
/// embedder (or a test) pins a profile.
///
/// Two folders, because they answer two different questions:
///
/// - **ephemeral** (a packaged app without `--persist`): one folder per
///   process. Nothing is meant to survive, and WebView2 refuses a second
///   process on one user-data folder — it fails with `HRESULT(0x80070057)`,
///   which is what happened when the same app was launched several times at
///   once. A per-PID folder makes concurrent launches independent.
/// - **persist**: one folder per app, named after a hash of the app's
///   identity. Sharing one folder between apps also shares an origin, so
///   app A's `localStorage` was readable by app B.
#[cfg(target_os = "windows")]
fn select_webview_profile(ephemeral: bool, app_id: &str) {
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_some() {
        return;
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Spid3r")
        .join("WebView2");
    prune_stale_ephemeral(&base.join("ephemeral"));
    let dir = webview_profile_path(&base, ephemeral, app_id);
    let _ = std::fs::create_dir_all(&dir);
    std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir);
}

/// The profile folder for one run, under `base` (`…\Spid3r\WebView2`).
#[cfg(target_os = "windows")]
fn webview_profile_path(base: &Path, ephemeral: bool, app_id: &str) -> PathBuf {
    if ephemeral {
        base.join("ephemeral").join(std::process::id().to_string())
    } else {
        base.join("persist").join(app_id)
    }
}

/// Best effort: drop ephemeral profiles left behind by earlier runs. Every
/// error is ignored — a folder that is still in use cannot be removed on
/// Windows, and a stale folder costs disk space, never correctness.
#[cfg(target_os = "windows")]
fn prune_stale_ephemeral(ephemeral_root: &Path) {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(ephemeral_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
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

fn dispatch_one(runtime: &Runtime, event: &str, payload: &str, entry: bridge::HandlerEntry) {
    let fallback_span = Span::new(Arc::from("<gui>"), 0, 0, 0);
    let span = entry.span.clone().unwrap_or(fallback_span);
    let args = match entry.arity {
        bridge::HandlerArity::OneArg => vec![Value::Str(payload.into())],
        bridge::HandlerArity::TwoArgs => {
            vec![Value::Str(event.into()), Value::Str(payload.into())]
        }
    };
    match runtime {
        Runtime::Tree(interp) => match interp.call_value(entry.handler, args, &span) {
            Ok(_) => {}
            Err(flow) => report_flow(flow),
        },
        Runtime::Vm(vm) => {
            if let Err(mut diag) = vm.borrow_mut().call_value_diag(&entry.handler, args) {
                if diag.span.as_ref().is_none_or(|s| s.line == 0) {
                    if let Some(ref handler_span) = entry.span {
                        diag.span = Some(handler_span.clone());
                    }
                }
                eprintln!("{}", diag);
            }
        }
    }
}

/// Dispatch every queued event to its handler, then flush the JavaScript the
/// handlers queued (handlers may queue more UI updates).
fn dispatch(runtime: &Runtime) {
    for _ in 0..256 {
        let events = bridge::take_event_entries();
        if events.is_empty() {
            break;
        }
        for (event, payload, entry) in events {
            dispatch_one(runtime, &event, &payload, entry);
        }
    }
}

/// Evaluate every queued JavaScript command in the page, in order.
fn flush(webview: &wry::WebView) {
    for js in bridge::take_out() {
        if let Err(e) = webview.evaluate_script(&js) {
            eprintln!("error[runtime]: cannot run script in the GUI page: {e}");
        }
    }
}

/// A stray control-flow value (return/break/continue outside a function) is a
/// bug in the handler; `Flow::Error` and `Flow::Fatal` are normal runtime
/// errors (the latter is execution-budget exhaustion).
fn report_flow(flow: Flow) {
    match flow {
        Flow::Error(d) | Flow::Fatal(d) => eprintln!("{}", d),
        other => eprintln!(
            "error[runtime]: GUI handler returned stray control flow: {}",
            match other {
                Flow::Value(_) => "value",
                Flow::Return(_) => "return",
                Flow::Break => "break",
                Flow::Continue => "continue",
                Flow::Error(_) | Flow::Fatal(_) => unreachable!(),
            }
        ),
    }
}

/// The GUI page served over the `spid3r://app/` custom protocol (B5).
///
/// `file://` page URLs crashed the process on the first page-to-host event:
/// wry's IPC handler parses the page URL with the `http` crate, which rejects
/// authority-less absolute URIs (`file:///C:/...`, `about:blank`). The
/// protocol gives every page a parseable origin and lets relative asset URLs
/// (css/js/images next to the `--html` file) load from disk.
struct StaticSite {
    /// Filesystem root every served path must stay inside. `None` serves the
    /// built-in default page only, or the in-memory `files` below.
    root: Option<PathBuf>,
    /// Path (relative to `root`, forward slashes) of the `--html` entry file.
    /// Served when the requested page path is `/`.
    entry: Option<String>,
    /// Assets embedded in the executable (`--payload`). When present these
    /// take precedence over `root` and are never written to disk.
    files: Option<Arc<std::collections::HashMap<String, Vec<u8>>>>,
}

impl StaticSite {
    /// A site for `--html page.html`: the root is the *current working
    /// directory* for pages under it (so sibling references like
    /// `ui/style.css` and `../data.json` resolve exactly as they did under
    /// `file://`), or the page's own directory for pages outside it.
    fn from_html_path(path: &str) -> Result<Self, String> {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("--html '{}': cannot read the current directory: {e}", path))?;
        Self::from_html_path_in(path, &cwd)
    }

    /// The body of [`Self::from_html_path`], with the base directory passed in
    /// so tests can exercise a relative `--html` without changing the process
    /// working directory (which is global and would race other tests).
    fn from_html_path_in(path: &str, cwd: &Path) -> Result<Self, String> {
        let abs = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        if !abs.is_file() {
            return Err(format!("--html '{}': file not found", path));
        }
        // Both sides must be canonical before they are compared: `serve_file`
        // re-canonicalises every request and requires it to stay under `root`,
        // so a non-canonical root rejects every asset — which is what a
        // symlinked or OneDrive-redirected directory used to do.
        let canonical = abs
            .canonicalize()
            .map_err(|e| format!("--html '{}': {e}", path))?;
        let cwd = cwd.canonicalize().map_err(|e| {
            format!(
                "--html '{}': cannot resolve the current directory: {e}",
                path
            )
        })?;
        let (root, entry) = match canonical.strip_prefix(&cwd) {
            // Under the working directory: keep the CWD root, so a reference
            // that climbs out of the page's folder still resolves the way it
            // did under `file://`. The entry is *derived* from the canonical
            // path rather than copied from the user's string — copying it put
            // `..` into the entry, which `serve_file` refuses as traversal, so
            // `--html ../site/page.html` served 403 for the page itself.
            Ok(rel) => (cwd, rel.to_string_lossy().replace('\\', "/")),
            // Outside the CWD (typically an absolute path): the page's own
            // directory is the root, so its assets stay reachable.
            Err(_) => {
                let root = canonical
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("/"));
                let entry = canonical
                    .file_name()
                    .map(|n| n.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                (root, entry)
            }
        };
        Ok(Self {
            root: Some(root),
            entry: Some(entry),
            files: None,
        })
    }

    /// A site served entirely from an embedded payload.
    fn from_memory(entry: String, files: Arc<std::collections::HashMap<String, Vec<u8>>>) -> Self {
        Self {
            root: None,
            entry: Some(entry),
            files: Some(files),
        }
    }

    /// The built-in page, with no filesystem access.
    fn builtin() -> Self {
        Self {
            root: None,
            entry: None,
            files: None,
        }
    }

    /// Read an asset (such as a window icon) from an embedded `--payload`
    /// archive if present, or from the site root on disk.
    fn read_asset(&self, spec: &str) -> Option<Vec<u8>> {
        let norm = spec.trim().replace('\\', "/");
        let norm = norm.strip_prefix("./").unwrap_or(&norm);
        if let Some(files) = &self.files {
            if let Some(b) = files.get(norm) {
                return Some(b.clone());
            }
        }
        if let Some(root) = &self.root {
            let candidate = root.join(spec);
            if candidate.is_file() {
                if let Ok(b) = std::fs::read(candidate) {
                    return Some(b);
                }
            }
        }
        None
    }

    /// The URL the webview navigates to. On Windows with `with_https_scheme(true)`,
    /// `wry` rewrites `spid3r://...` to `https://spid3r.app/...` for WebView2, so
    /// the page origin always parses as an `http::Uri` and is a secure context.
    fn page_url(&self) -> String {
        match &self.entry {
            Some(entry) => format!("spid3r://app/{}", encode_url_path(entry)),
            None => "spid3r://app/".to_string(),
        }
    }

    /// Fulfil one `spid3r://app/...` resource request.
    fn serve(&self, request: HttpRequest<Vec<u8>>) -> HttpResponse<Cow<'static, [u8]>> {
        let path = request.uri().path();
        let Some(rel) = percent_decode(path) else {
            return Self::text_response(400, "bad request: malformed percent-encoding");
        };
        let rel = rel.trim_start_matches('/');
        // The bare `/` page is the entry document (or the built-in page).
        if rel.is_empty() {
            match &self.entry {
                Some(entry) => return self.serve_path(entry),
                None => return Self::html_response(DEFAULT_PAGE),
            }
        }
        self.serve_path(rel)
    }

    /// Resolve one request against the embedded payload, then the disk root.
    fn serve_path(&self, rel: &str) -> HttpResponse<Cow<'static, [u8]>> {
        if let Some(files) = &self.files {
            return match files.get(rel) {
                Some(bytes) => HttpResponse::builder()
                    .status(200)
                    .header("Content-Type", content_type(rel))
                    .body(Cow::Owned(bytes.clone()))
                    .expect("static response builder"),
                None => Self::text_response(404, "not found"),
            };
        }
        match &self.root {
            Some(root) => self.serve_file(root, rel),
            None => Self::text_response(404, "not found"),
        }
    }

    /// Read one file under `root`, rejecting anything that escapes it.
    fn serve_file(&self, root: &Path, rel: &str) -> HttpResponse<Cow<'static, [u8]>> {
        if rel.split('/').any(|seg| seg == "..") {
            return Self::text_response(403, "forbidden");
        }
        let target = root.join(rel);
        let Ok(canonical) = target.canonicalize() else {
            return Self::text_response(404, "not found");
        };
        if !canonical.starts_with(root) {
            return Self::text_response(403, "forbidden");
        }
        match std::fs::read(&canonical) {
            Ok(bytes) => HttpResponse::builder()
                .status(200)
                .header("Content-Type", content_type(rel))
                .body(Cow::Owned(bytes))
                .expect("static response builder"),
            Err(_) => Self::text_response(404, "not found"),
        }
    }

    fn html_response(html: &'static str) -> HttpResponse<Cow<'static, [u8]>> {
        HttpResponse::builder()
            .status(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Cow::Borrowed(html.as_bytes()))
            .expect("static response builder")
    }

    fn text_response(status: u16, text: &'static str) -> HttpResponse<Cow<'static, [u8]>> {
        HttpResponse::builder()
            .status(status)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Cow::Borrowed(text.as_bytes()))
            .expect("static response builder")
    }
}

/// Content-Type for a served path, by extension. Unknown extensions fall back
/// to `application/octet-stream`, which the webview downloads instead of
/// rendering — the common web assets are all covered explicitly.
fn content_type(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" | "matrix" | "mt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Percent-encode a path for use inside a URL: everything outside the URL
/// unreserved set plus `/` is encoded, byte by byte. Non-ASCII characters
/// (OneDrive usernames, CJK directories) MUST be encoded — the `http` crate
/// rejects raw non-ASCII in a URI, and wry's IPC handler unwraps that parse.
fn encode_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Decode `%XX` sequences in a URL path. Returns `None` on a malformed
/// escape instead of guessing.
fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // Paths from the webview are ASCII (we encoded them), but a query string
    // or a future caller could hand us UTF-8; only valid UTF-8 is accepted.
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B5 contract: every URL the GUI layer navigates to must parse as an
    /// `http::Uri`. wry's IPC handler does
    /// `Request::builder().uri(page_source).unwrap()`, so an unparseable page
    /// URL aborts the whole process the moment the page emits an event.
    #[test]
    fn page_urls_are_http_uri_parseable() {
        let wry_http = "spid3r://app/index.html".parse::<wry::http::Uri>().unwrap();
        assert_eq!(wry_http.authority().map(|a| a.as_str()), Some("app"));
        // The Windows workaround form wry navigates internally.
        "http://spid3r.app/index.html"
            .parse::<wry::http::Uri>()
            .expect("workaround origin must parse");
        // The built-in page has a parseable origin too (no about:blank).
        "spid3r://app/".parse::<wry::http::Uri>().unwrap();
        // And the reason file:// is gone: the http crate rejects it.
        assert!("file:///C:/Users/x/page.html"
            .parse::<wry::http::Uri>()
            .is_err());
    }

    #[test]
    fn builtin_site_serves_default_page() {
        let site = StaticSite::builtin();
        assert_eq!(site.page_url(), "spid3r://app/");
        let resp = site.serve_request("/");
        assert_eq!(resp.status(), 200);
        assert!(resp.body().starts_with(b"<!doctype html>"));
        // No root: nothing else is served.
        assert_eq!(site.serve_request("/anything.html").status(), 404);
    }

    #[test]
    fn file_site_serves_entry_and_blocks_traversal() {
        let dir = std::env::temp_dir().join("spid3r_gui_site_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(dir.join("ui/index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(dir.join("ui/style.css"), "h1{}").unwrap();
        std::fs::write(dir.join("secret.txt"), "top").unwrap();

        // Absolute --html path: the site root is the file's own directory.
        let site = StaticSite::from_html_path(dir.join("ui/index.html").to_string_lossy().as_ref())
            .unwrap();
        // The entry document is served at "/" ...
        let resp = site.serve_request("/");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body().as_ref(), b"<h1>hi</h1>");
        // ... and assets relative to it, with the right content type ...
        let css = site.serve_request("/style.css");
        assert_eq!(css.status(), 200);
        assert_eq!(
            css.headers().get("Content-Type").unwrap(),
            "text/css; charset=utf-8"
        );
        // ... while traversal above the root is refused.
        assert_eq!(site.serve_request("/../secret.txt").status(), 403);
        assert_eq!(
            site.serve_request("/%2e%2e/secret.txt").status(),
            403,
            "encoded traversal must be decoded before the check"
        );
        // Spaces in file names must survive the URL round trip.
        std::fs::write(dir.join("ui/with space.html"), "x").unwrap();
        assert_eq!(site.serve_request("/with%20space.html").status(), 200);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--html ../site/page.html` is a relative path that climbs out of the
    /// working directory. The old code copied the user's string into `entry`,
    /// so the entry document itself was requested as `/../site/page.html`,
    /// which `serve_file` refuses as traversal: the webview showed a blank
    /// window and nothing was reported. The entry must be derived from the
    /// canonical path instead.
    #[test]
    fn relative_html_outside_the_cwd_serves_its_entry() {
        let base = std::env::temp_dir().join("spid3r_gui_relative_html_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::create_dir_all(base.join("site")).unwrap();
        std::fs::write(base.join("site/page.html"), "<h1>site</h1>").unwrap();
        std::fs::write(base.join("site/style.css"), "h1{}").unwrap();

        // Run from `sub`, point at a page one level up.
        let cwd = base.join("sub");
        let site = StaticSite::from_html_path_in("../site/page.html", &cwd).unwrap();
        let entry = site.entry.clone().expect("entry");
        assert!(
            !entry.contains(".."),
            "the entry must never carry a `..` segment: {entry}"
        );
        let resp = site.serve_request("/");
        assert_eq!(resp.status(), 200, "entry document must load");
        assert_eq!(resp.body().as_ref(), b"<h1>site</h1>");
        // The page's own folder is the root, so its assets resolve.
        assert_eq!(site.serve_request("/style.css").status(), 200);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A page under the CWD keeps the CWD root, so a reference that climbs out
    /// of the page's folder (`../data.json`) still resolves the way it did
    /// under `file://`. The root and the entry are both canonical, which is
    /// what lets `serve_file`'s `starts_with(root)` check pass for assets.
    #[test]
    fn page_under_the_cwd_keeps_the_cwd_root() {
        let base = std::env::temp_dir().join("spid3r_gui_cwd_root_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ui")).unwrap();
        std::fs::write(base.join("ui/index.html"), "<h1>ui</h1>").unwrap();
        std::fs::write(base.join("data.json"), "{\"ok\":true}").unwrap();

        let site = StaticSite::from_html_path_in("ui/index.html", &base).unwrap();
        assert_eq!(site.entry.as_deref(), Some("ui/index.html"));
        assert_eq!(site.serve_request("/").status(), 200);
        // `../data.json` from `/ui/index.html` resolves to `/data.json`.
        let json = site.serve_request("/data.json");
        assert_eq!(json.status(), 200);
        assert_eq!(json.body().as_ref(), b"{\"ok\":true}");

        // A path that uses `..` to *stay* under the CWD is normalised, not
        // refused.
        let nested = StaticSite::from_html_path_in("../ui/index.html", &base.join("ui")).unwrap();
        assert_eq!(nested.entry.as_deref(), Some("index.html"));
        assert_eq!(nested.serve_request("/").status(), 200);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Two apps must not share a persisted profile (one profile means one
    /// origin, so app A's `localStorage` would be readable by app B), and an
    /// ephemeral profile must be per-process — WebView2 refuses a second
    /// process on one user-data folder.
    #[test]
    #[cfg(target_os = "windows")]
    fn webview_profiles_are_per_app_and_per_process() {
        let base = Path::new(r"C:\Users\x\AppData\Local\Spid3r\WebView2");
        let a = webview_profile_path(base, false, "aaaa000000000000");
        let b = webview_profile_path(base, false, "bbbb111111111111");
        assert_ne!(a, b, "different apps need different profiles");
        assert!(a.ends_with(Path::new("persist").join("aaaa000000000000")));
        // Same app, same profile: `--persist` must survive a restart.
        assert_eq!(a, webview_profile_path(base, false, "aaaa000000000000"));

        let ephemeral = webview_profile_path(base, true, "aaaa000000000000");
        assert!(ephemeral.ends_with(std::process::id().to_string()));
        assert!(!ephemeral.starts_with(base.join("persist")));
    }

    impl StaticSite {
        /// Test helper: build a request the way wry hands one over.
        fn serve_request(&self, path: &str) -> HttpResponse<Cow<'static, [u8]>> {
            let request = HttpRequest::builder()
                .uri(format!("spid3r://app{}", path))
                .body(Vec::new())
                .unwrap();
            self.serve(request)
        }
    }

    #[test]
    fn webview_navigation_allowlist_enforces_exact_origin_boundary() {
        for ok in [
            "about:blank",
            "spid3r://app",
            "spid3r://app/",
            "spid3r://app/index.html",
            "spid3r://app/control.html?tab=1",
        ] {
            assert!(
                is_allowed_webview_url_with_https(ok, true),
                "should allow (win): {}",
                ok
            );
            assert!(
                is_allowed_webview_url_with_https(ok, false),
                "should allow (unix): {}",
                ok
            );
        }
        // `https://spid3r.app` is ONLY allowed on Windows where
        // `with_https_scheme(true)` intercepts it; on non-Windows it must be
        // rejected so a page cannot navigate to the public domain.
        for win_only in [
            "https://spid3r.app",
            "https://spid3r.app/",
            "https://spid3r.app/index.html",
            "https://spid3r.app?q=1",
            "https://spid3r.app#section",
        ] {
            assert!(
                is_allowed_webview_url_with_https(win_only, true),
                "should allow on Windows: {}",
                win_only
            );
            assert!(
                !is_allowed_webview_url_with_https(win_only, false),
                "must reject on non-Windows: {}",
                win_only
            );
        }
        for bad in [
            "https://spid3r.app.evil.com/",
            "https://spid3r.app@evil.com/",
            "https://spid3r.app:8443/",
            "http://spid3r.app/",
            "https://evil.com/",
            "spid3r://evil.com/",
            "spid3r://app.evil.com/",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///C:/Windows/win.ini",
        ] {
            assert!(
                !is_allowed_webview_url_with_https(bad, true),
                "should reject (win): {}",
                bad
            );
            assert!(
                !is_allowed_webview_url_with_https(bad, false),
                "should reject (unix): {}",
                bad
            );
        }
    }
}

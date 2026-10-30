//! The window half of the GUI layer: a `tao` window with a `wry` webview,
//! plus the headless `--dry-run` replay the tests assert against.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wry::http::{Request as HttpRequest, Response as HttpResponse};

use crate::diagnostics::Span;
use crate::interpreter::Flow;
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

/// Wakes the window's event loop when the page emits an event.
pub enum Msg {
    Event,
}

/// Replay the bridge without a window: the `ready` event is dispatched and
/// the queued JavaScript is printed (`js: …`) instead of evaluated.
pub fn dry_run(runtime: &Runtime) {
    bridge::push_event("ready", "");
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
) -> Result<(), String> {
    let title = bridge::take_title().unwrap_or_else(|| "spid3r gui".to_string());

    // The CLI runs every subcommand on a big-stack worker thread (deep AST
    // recursion does not fit the 1 MiB main stack), so the event loop is
    // usually NOT on the main thread. tao 0.34 aborts at build() unless this
    // is set; the calculator launcher spawns a worker for the same stack
    // reason.
    #[cfg(windows)]
    let event_loop = {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        let mut builder = tao::event_loop::EventLoopBuilder::<Msg>::with_user_event();
        builder.with_any_thread(true);
        builder.build()
    };
    #[cfg(not(windows))]
    let event_loop = tao::event_loop::EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[cfg(target_os = "windows")]
    select_webview_profile(ephemeral, app_id);
    #[cfg(not(target_os = "windows"))]
    let _ = app_id;

    let window = tao::window::WindowBuilder::new()
        .with_title(title)
        .with_inner_size(tao::dpi::LogicalSize::new(920.0, 640.0))
        .build(&event_loop)
        .map_err(|e| format!("cannot open the GUI window: {e}"))?;

    let mut builder = wry::WebViewBuilder::new()
        .with_initialization_script(INIT_SCRIPT)
        .with_incognito(ephemeral)
        .with_ipc_handler({
            let proxy = proxy.clone();
            move |request: wry::http::Request<String>| {
                let body = request.into_body();
                let event = bridge::json_field(&body, "event").unwrap_or_default();
                let payload = bridge::json_field(&body, "payload").unwrap_or_default();
                bridge::push_event(&event, &payload);
                // The loop is waiting; nudge it to dispatch.
                let _ = proxy.send_event(Msg::Event);
            }
        })
        .with_on_page_load_handler({
            let proxy = proxy.clone();
            move |event: wry::PageLoadEvent, _url: String| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    bridge::push_event("ready", "");
                    let _ = proxy.send_event(Msg::Event);
                }
            }
        });

    // The page is served through the `spid3r://app/` custom protocol instead
    // of `file://` (B5): wry's IPC handler builds an `http::Request` whose URI
    // is the page's own URL and `unwrap()`s the parse, and the `http` crate
    // rejects any absolute URI without an authority — `file:///C:/...` and
    // `about:blank` both fail, so the FIRST `window.spid3r.emit(...)` from a
    // `--html` page aborted the process (0xC0000409). Under the protocol the
    // page origin is `spid3r://app/...` (Windows workaround form:
    // `http://spid3r.app/...`), which always parses. The custom-protocol
    // handler also serves relative assets (css/js/images) from disk.
    let site = Arc::new(match source {
        PageSource::File(path) => StaticSite::from_html_path(&path)?,
        PageSource::Memory { entry, files } => StaticSite::from_memory(entry, files),
        PageSource::Builtin => StaticSite::builtin(),
    });
    builder = builder.with_custom_protocol("spid3r".to_string(), {
        let site = Arc::clone(&site);
        move |_id, request| site.serve(request)
    });
    builder = builder.with_url(site.page_url());

    let webview = builder.build(&window).map_err(|e| {
        format!("cannot start the webview (is the WebView2 runtime installed?): {e}")
    })?;

    // The loop owns the engine handle and the webview from here on.
    event_loop.run(move |event, _, control_flow| {
        *control_flow = tao::event_loop::ControlFlow::Wait;
        match event {
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } => *control_flow = tao::event_loop::ControlFlow::Exit,
            tao::event::Event::UserEvent(Msg::Event) => {
                dispatch(&runtime);
                flush(&webview);
            }
            _ => {}
        }
    });
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

/// Dispatch every queued event to its handler, then flush the JavaScript the
/// handlers queued (handlers may queue more UI updates).
fn dispatch(runtime: &Runtime) {
    for _ in 0..256 {
        let events = bridge::take_events();
        if events.is_empty() {
            break;
        }
        for (event, payload, handler) in events {
            let span = Span::new(Arc::from("<gui>"), 0, 0, 0);
            let args = vec![Value::Str(event.into()), Value::Str(payload.into())];
            match runtime {
                Runtime::Tree(interp) => match interp.call_value(handler, args, &span) {
                    Ok(_) => {}
                    Err(flow) => report_flow(flow),
                },
                // The VM reports failures as a message; there is no `Flow`
                // channel to unwind through because the handler is a compiled
                // function, not a tree node.
                Runtime::Vm(vm) => {
                    if let Err(msg) = vm.borrow_mut().call_value(&handler, args) {
                        eprintln!("error[runtime]: {}", msg);
                    }
                }
            }
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

    /// The URL the webview navigates to. `wry` rewrites `spid3r://...` to
    /// `http://spid3r.app/...` for WebView2, so the page origin always parses
    /// as an `http::Uri`.
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
        "txt" | "matrix" | "md" => "text/plain; charset=utf-8",
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
}

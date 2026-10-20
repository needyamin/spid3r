//! The window half of the GUI layer: a `tao` window with a `wry` webview,
//! plus the headless `--dry-run` replay the tests assert against.

use std::sync::Arc;

use crate::diagnostics::Span;
use crate::interpreter::{Flow, Interp};
use crate::value::Value;

use super::bridge;

/// The page shown when no `--html` page is given. `#spid3r-root` is the
/// element `gui_set_html` replaces.
const DEFAULT_PAGE: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>spid3r gui</title></head>
<body style="font:14px/1.5 system-ui,sans-serif;margin:2rem">
  <div id="spid3r-root">
    <p>The spid3r GUI window is running. Build the UI from your program with
    <code>gui_set_html(...)</code> / <code>gui_eval(...)</code>,
    and react to the page with <code>gui_on(event, handler)</code>.</p>
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
pub fn dry_run(interp: &Interp) {
    bridge::push_event("ready", "");
    dispatch(interp);
    for js in bridge::take_out() {
        println!("js: {}", js);
    }
}

/// Open the GUI window and run its event loop until the window closes.
/// Returns only if the window could not be created at all.
pub fn open(interp: &Arc<Interp>, html: Option<&str>) -> Result<(), String> {
    let title = bridge::take_title().unwrap_or_else(|| "spid3r gui".to_string());

    let event_loop = tao::event_loop::EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[cfg(target_os = "windows")]
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let spid3r_webview = local_app_data.join("Spid3r").join("WebView2");
        let _ = std::fs::create_dir_all(&spid3r_webview);
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", spid3r_webview);
    }

    let window = tao::window::WindowBuilder::new()
        .with_title(title)
        .with_inner_size(tao::dpi::LogicalSize::new(920.0, 640.0))
        .build(&event_loop)
        .map_err(|e| format!("cannot open the GUI window: {e}"))?;

    let mut builder = wry::WebViewBuilder::new()
        .with_initialization_script(INIT_SCRIPT)
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

    builder = match html {
        Some(path) => builder.with_url(file_url(path)?),
        None => builder.with_html(DEFAULT_PAGE),
    };

    let webview = builder.build(&window).map_err(|e| {
        format!("cannot start the webview (is the WebView2 runtime installed?): {e}")
    })?;

    // The loop owns the interpreter handle and the webview from here on.
    let interp = Arc::clone(interp);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = tao::event_loop::ControlFlow::Wait;
        match event {
            tao::event::Event::WindowEvent {
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } => *control_flow = tao::event_loop::ControlFlow::Exit,
            tao::event::Event::UserEvent(Msg::Event) => {
                dispatch(&interp);
                flush(&webview);
            }
            _ => {}
        }
    });
}

/// Dispatch every queued event to its handler, then flush the JavaScript the
/// handlers queued (handlers may queue more UI updates).
fn dispatch(interp: &Interp) {
    for _ in 0..256 {
        let events = bridge::take_events();
        if events.is_empty() {
            break;
        }
        for (event, payload, handler) in events {
            let span = Span::new(Arc::from("<gui>"), 0, 0, 0);
            match interp.call_value(
                handler,
                vec![Value::Str(event.into()), Value::Str(payload.into())],
                &span,
            ) {
                Ok(_) => {}
                Err(flow) => report_flow(flow),
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
/// bug in the handler; a `Flow::Error` is a normal runtime error.
fn report_flow(flow: Flow) {
    match flow {
        Flow::Error(d) => eprintln!("{}", d),
        other => eprintln!(
            "error[runtime]: GUI handler returned stray control flow: {}",
            match other {
                Flow::Value(_) => "value",
                Flow::Return(_) => "return",
                Flow::Break => "break",
                Flow::Continue => "continue",
                Flow::Error(_) => unreachable!(),
            }
        ),
    }
}

/// Turn a filesystem path into a `file://` URL for the webview.
fn file_url(path: &str) -> Result<String, String> {
    let abs = match std::path::Path::new(path).is_absolute() {
        true => std::path::PathBuf::from(path),
        false => std::env::current_dir()
            .map_err(|e| format!("cannot resolve '{}': {e}", path))?
            .join(path),
    };
    let text = abs.to_string_lossy().replace('\\', "/");
    // Minimal percent-encoding for the characters that break URLs.
    let mut encoded = String::with_capacity(text.len() + 8);
    if !text.starts_with('/') {
        encoded.push('/');
    }
    for c in text.chars() {
        match c {
            ' ' => encoded.push_str("%20"),
            '"' => encoded.push_str("%22"),
            '<' => encoded.push_str("%3c"),
            '>' => encoded.push_str("%3e"),
            '#' => encoded.push_str("%23"),
            '?' => encoded.push_str("%3f"),
            c => encoded.push(c),
        }
    }
    Ok(format!("file://{}", encoded))
}

//! Application packager and cross-platform bundler for Spid3r.
//!
//! Supports bundling Spid3r scripts and applications for:
//! - WebAssembly / Browser (`--target wasm`): Standalone web directory or single-file HTML.
//! - Desktop (`--target desktop`): Precompiled `.matrixc` bytecode and desktop asset bundles.
//! - Mobile (`--target android`): Complete Android Studio WebView application project.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::diagnostics::{Diagnostic, Phase};
use crate::parser;
use crate::semantic;
use crate::vm::compile_program;

const EMBEDDED_WASM_BYTES: &[u8] = include_bytes!("../web/spid3r.wasm");
const EMBEDDED_JS_SDK: &str = include_str!("../web/spid3r.js");

/// Target platform for bundling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleTarget {
    Wasm,
    Desktop,
    Android,
}

impl BundleTarget {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "wasm" | "web" | "browser" => Ok(BundleTarget::Wasm),
            "desktop" | "windows" | "linux" | "macos" => Ok(BundleTarget::Desktop),
            "android" | "mobile" => Ok(BundleTarget::Android),
            other => Err(format!(
                "unsupported target '{}'; valid targets are 'wasm', 'desktop', 'android'",
                other
            )),
        }
    }
}

/// Options parsed from command-line arguments.
#[derive(Debug, Clone)]
pub struct BundleOptions {
    pub source_file: PathBuf,
    pub target: BundleTarget,
    pub html_file: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub single_file: bool,
    /// Desktop bundles only: copy the Spid3r runtime executable into the
    /// bundle and point the launcher at it, so the app runs on a machine
    /// with no `spid3r` on PATH (B4).
    pub self_contained: bool,
}

/// Helper to base64 encode byte buffers without external dependencies.
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };

        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);

        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Main entry point for `spid3r bundle`.
pub fn bundle(opts: &BundleOptions) -> Result<PathBuf, Diagnostic> {
    if opts.single_file && opts.target != BundleTarget::Wasm {
        return Err(Diagnostic::new(
            Phase::Usage,
            "--single-file applies only to --target wasm",
        ));
    }
    if opts.self_contained && opts.target != BundleTarget::Desktop {
        return Err(Diagnostic::new(
            Phase::Usage,
            "--self-contained applies only to --target desktop",
        ));
    }

    if !opts.source_file.is_file() {
        return Err(Diagnostic::new(
            Phase::Semantic,
            format!("source file '{}' not found", opts.source_file.display()),
        ));
    }

    let src = fs::read_to_string(&opts.source_file).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!(
                "failed to read source '{}': {}",
                opts.source_file.display(),
                e
            ),
        )
    })?;

    // Static verify the script
    let file_id: Arc<str> = Arc::from(opts.source_file.to_string_lossy());
    let program = parser::parse_file(&src, file_id.clone())?;
    let has_main = program.stmts.iter().any(|s| match s {
        crate::ast::Stmt::Fn(f) => f.name.as_deref() == Some("main"),
        _ => false,
    });
    semantic::check_file(&program, has_main, &file_id)?;

    match opts.target {
        BundleTarget::Wasm => bundle_wasm(opts, &src),
        BundleTarget::Desktop => bundle_desktop(opts, &program),
        BundleTarget::Android => bundle_android(opts, &src),
    }
}

// ---------------------------------------------------------------- WASM BUNDLER

fn bundle_wasm(opts: &BundleOptions, src: &str) -> Result<PathBuf, Diagnostic> {
    let stem = opts
        .source_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app");

    if opts.single_file
        || opts
            .output_path
            .as_ref()
            .map(|p| p.extension().and_then(|e| e.to_str()) == Some("html"))
            .unwrap_or(false)
    {
        let out_html_path = {
            // C-03: a single-file bundle IS an HTML document; without the
            // extension the OS has no reason to open it in a browser and the
            // file renders as text. Respect an explicit extension, append the
            // canonical one when `-o` gave none.
            let p = opts
                .output_path
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("{}.html", stem)));
            if p.extension().is_none() {
                PathBuf::from(format!("{}.html", p.to_string_lossy()))
            } else {
                p
            }
        };
        let base64_wasm = base64_encode(EMBEDDED_WASM_BYTES);

        let user_html_custom = if let Some(ref html_p) = opts.html_file {
            fs::read_to_string(html_p).map_err(|e| {
                Diagnostic::new(
                    Phase::Semantic,
                    format!("failed to read HTML template '{}': {}", html_p.display(), e),
                )
            })?
        } else {
            default_wasm_html_template(stem)
        };

        let single_file_html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{}</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; margin: 0; padding: 20px; background: #0b0f17; color: #f1f5f9; }}
    h1 {{ color: #38bdf8; font-size: 1.5rem; margin-bottom: 8px; }}
    #terminal {{ background: #05080e; border: 1px solid rgba(255,255,255,0.1); border-radius: 8px; padding: 16px; font-family: monospace; white-space: pre-wrap; min-height: 240px; color: #38bdf8; }}
  </style>
</head>
<body>
  <h1>⚡ Spid3r Standalone Web Bundle</h1>
  <div id="terminal">Initializing Spid3r runtime...</div>

  {}

  <script id="spid3r-source" type="application/spid3r">
{}
  </script>

  <script>
{}
  </script>

  <script>
    (async function() {{
      const term = document.getElementById('terminal');
      try {{
        const b64 = "{}";
        const binStr = atob(b64);
        const len = binStr.length;
        const bytes = new Uint8Array(len);
        for (let i = 0; i < len; i++) {{
          bytes[i] = binStr.charCodeAt(i);
        }}
        const sp = await Spid3r.init(bytes);
        const src = document.getElementById('spid3r-source').textContent.trim();
        const res = sp.eval(src, 'vm');
        term.innerText = res.stdout || (res.ok ? "Program executed cleanly with exit code " + res.exit_code : "Error: " + res.error);
      }} catch (err) {{
        term.innerText = "Fatal execution error: " + err.message;
      }}
    }})();
  </script>
</body>
</html>"#,
            escape_markup(stem),
            user_html_custom,
            // HTML end tags are case-insensitive and tolerate whitespace, so
            // a plain `</script>` replace is not enough to keep the program
            // source inside the <script type="application/spid3r"> element;
            // neutralize every `<` of a closing-tag sequence instead.
            src.replace("</", "<\\/"),
            EMBEDDED_JS_SDK,
            base64_wasm
        );

        // `-o dist/app.html` is the documented invocation, and the directory
        // targets create their output directory — this branch used to write
        // straight to the path, so it failed with "cannot find the path" on a
        // fresh checkout unless `dist/` already existed.
        if let Some(parent) = out_html_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                fs::create_dir_all(parent).map_err(|e| {
                    Diagnostic::new(
                        Phase::Semantic,
                        format!("failed to create '{}': {}", parent.display(), e),
                    )
                })?;
            }
        }
        fs::write(&out_html_path, single_file_html).map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!(
                    "failed to write HTML bundle '{}': {}",
                    out_html_path.display(),
                    e
                ),
            )
        })?;
        Ok(out_html_path)
    } else {
        let out_dir = opts
            .output_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("{}_web", stem)));
        fs::create_dir_all(&out_dir).map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!("failed to create output dir '{}': {}", out_dir.display(), e),
            )
        })?;

        // 1. Copy spid3r.wasm
        fs::write(out_dir.join("spid3r.wasm"), EMBEDDED_WASM_BYTES).map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!("failed to write spid3r.wasm: {}", e),
            )
        })?;

        // 2. Copy spid3r.js
        fs::write(out_dir.join("spid3r.js"), EMBEDDED_JS_SDK).map_err(|e| {
            Diagnostic::new(Phase::Semantic, format!("failed to write spid3r.js: {}", e))
        })?;

        // 3. Copy app source
        fs::write(out_dir.join("app.matrix"), src).map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!("failed to write app.matrix: {}", e),
            )
        })?;

        // 4. Write index.html
        let index_html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{}</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; margin: 0; padding: 24px; background: #0b0f17; color: #f1f5f9; }}
    h1 {{ color: #38bdf8; font-size: 1.5rem; }}
    #terminal {{ background: #05080e; border: 1px solid rgba(255,255,255,0.1); border-radius: 8px; padding: 16px; font-family: monospace; white-space: pre-wrap; min-height: 240px; color: #38bdf8; margin-top: 16px; }}
  </style>
</head>
<body>
  <h1>⚡ Spid3r Application</h1>
  <div id="terminal">Loading Spid3r WebAssembly runtime...</div>

  <script src="spid3r.js"></script>
  <script>
    (async function() {{
      const term = document.getElementById('terminal');
      try {{
        const sp = await Spid3r.init("spid3r.wasm");
        const resp = await fetch("app.matrix");
        const code = await resp.text();
        const res = sp.eval(code, "vm");
        term.innerText = res.stdout || (res.ok ? "Program executed cleanly with exit code " + res.exit_code : "Error: " + res.error);
      }} catch (err) {{
        term.innerText = "Fatal execution error: " + err.message;
      }}
    }})();
  </script>
</body>
</html>"#,
            escape_markup(stem)
        );
        fs::write(out_dir.join("index.html"), index_html).map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!("failed to write index.html: {}", e),
            )
        })?;

        Ok(out_dir)
    }
}

fn default_wasm_html_template(_stem: &str) -> String {
    String::new()
}

/// Escape text interpolated into generated HTML/XML. The source *program* is
/// escaped at its embedding sites; the file stem used to come through raw,
/// which let a POSIX filename containing `<`, `>` or `"` break out of the
/// `<title>` element or the Android `android:label` attribute.
fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ------------------------------------------------------------- DESKTOP BUNDLER

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_child = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_child)?;
        } else {
            fs::copy(entry.path(), dst_child)?;
        }
    }
    Ok(())
}

fn uses_gui_natives(program: &crate::ast::Program) -> bool {
    const GUI_NATIVES: &[&str] = &[
        "gui_eval",
        "gui_set_html",
        "gui_on",
        "gui_emit",
        "gui_title",
    ];
    fn check_expr(e: &crate::ast::Expr) -> bool {
        match e {
            crate::ast::Expr::Ident(name, _) => GUI_NATIVES.contains(&name.as_str()),
            crate::ast::Expr::Call { callee, args, .. } => {
                check_expr(callee) || args.iter().any(check_expr)
            }
            crate::ast::Expr::Method { object, args, .. } => {
                check_expr(object) || args.iter().any(check_expr)
            }
            crate::ast::Expr::Unary { expr, .. } => check_expr(expr),
            crate::ast::Expr::Binary { l, r, .. } | crate::ast::Expr::Logical { l, r, .. } => {
                check_expr(l) || check_expr(r)
            }
            crate::ast::Expr::Assign { target, value, .. } => {
                check_expr(target) || check_expr(value)
            }
            crate::ast::Expr::List(items, _) => items.iter().any(check_expr),
            crate::ast::Expr::Index { object, index, .. } => {
                check_expr(object) || check_expr(index)
            }
            crate::ast::Expr::Field { object, .. } => check_expr(object),
            crate::ast::Expr::OptField { object, .. } => check_expr(object),
            crate::ast::Expr::New { args, .. } => args.iter().any(check_expr),
            crate::ast::Expr::Await(expr, _) => check_expr(expr),
            crate::ast::Expr::Spawn { callable, .. } => check_expr(callable),
            crate::ast::Expr::Fn(proto) => check_block(&proto.body),
            crate::ast::Expr::NamedArg { value, .. } => check_expr(value),
            crate::ast::Expr::Propagate(expr, _) => check_expr(expr),
            crate::ast::Expr::SuperCall { args, .. } => args.iter().any(check_expr),
            crate::ast::Expr::If {
                cond, then, els, ..
            } => {
                check_expr(cond)
                    || check_block(then)
                    || els.as_ref().map(check_block).unwrap_or(false)
            }
            crate::ast::Expr::Match {
                scrutinee, arms, ..
            } => {
                check_expr(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().map(check_expr).unwrap_or(false)
                            || check_block(&arm.body)
                    })
            }
            crate::ast::Expr::Coalesce { l, r, .. } => check_expr(l) || check_expr(r),
            _ => false,
        }
    }
    fn check_block(b: &crate::ast::Block) -> bool {
        b.stmts.iter().any(check_stmt)
    }
    fn check_stmt(s: &crate::ast::Stmt) -> bool {
        match s {
            crate::ast::Stmt::Expr(e) => check_expr(e),
            crate::ast::Stmt::Let { value: Some(e), .. }
            | crate::ast::Stmt::Const { value: e, .. } => check_expr(e),
            crate::ast::Stmt::LetTuple { value, .. } => check_expr(value),
            crate::ast::Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                check_expr(cond)
                    || check_block(then)
                    || elifs.iter().any(|(c, b)| check_expr(c) || check_block(b))
                    || els.as_ref().map(check_block).unwrap_or(false)
            }
            crate::ast::Stmt::While { cond, body, .. } => check_expr(cond) || check_block(body),
            crate::ast::Stmt::ForIn { iter, body, .. } => check_expr(iter) || check_block(body),
            crate::ast::Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => check_stmt(init) || check_expr(cond) || check_expr(post) || check_block(body),
            crate::ast::Stmt::Return { value: Some(e), .. } => check_expr(e),
            crate::ast::Stmt::Throw { value, .. } => check_expr(value),
            crate::ast::Stmt::Fn(f) => check_block(&f.body),
            crate::ast::Stmt::Class(c) => c.methods.iter().any(|m| check_block(&m.body)),
            crate::ast::Stmt::Block(b) => check_block(b),
            crate::ast::Stmt::Spawn { callable, .. } => check_expr(callable),
            crate::ast::Stmt::Yield { value, .. } => check_expr(value),
            crate::ast::Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                check_block(body)
                    || catch
                        .as_ref()
                        .map(|c| check_block(&c.body))
                        .unwrap_or(false)
                    || finally.as_ref().map(check_block).unwrap_or(false)
            }
            _ => false,
        }
    }
    program.stmts.iter().any(check_stmt)
}

fn bundle_desktop(
    opts: &BundleOptions,
    program: &crate::ast::Program,
) -> Result<PathBuf, Diagnostic> {
    let stem = opts
        .source_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app");

    let out_dir = opts
        .output_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}_desktop", stem)));
    fs::create_dir_all(&out_dir).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!(
                "failed to create desktop bundle dir '{}': {}",
                out_dir.display(),
                e
            ),
        )
    })?;

    // 1. Compile bytecode with all modules embedded
    let dirs = crate::modules::default_module_dirs();
    let (vfunc, _public) =
        compile_program(program, opts.source_file.to_string_lossy().as_ref(), &dirs)?;
    let bytecode_bytes = crate::vm::bytecode::serialize_vmfunc(&vfunc);
    let matrixc_path = out_dir.join(format!("{}.matrixc", stem));
    fs::write(&matrixc_path, bytecode_bytes).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write compiled bytecode: {}", e),
        )
    })?;

    // Also copy the source .matrix file so `spid3r gui` can run it
    let matrix_src_path = out_dir.join(format!("{}.matrix", stem));
    let _ = fs::copy(&opts.source_file, &matrix_src_path);

    // 2. If HTML frontend provided, copy it preserving relative asset structure
    let mut html_arg: Option<String> = None;
    if let Some(ref html_p) = opts.html_file {
        let src_dir = opts.source_file.parent().unwrap_or(Path::new("."));
        // Resolve a relative `--html` beside the source file first — the same
        // base `--embed` uses — then against the working directory, so both
        // `spid3r bundle app.matrix --html ui/index.html` and a run from the
        // page's own folder work. A file found in neither place is an error:
        // the old code skipped it silently and emitted a *console* bundle with
        // no UI at all, which looked like a successful build.
        let resolved = if html_p.is_absolute() {
            html_p.clone()
        } else if src_dir.join(html_p).is_file() {
            src_dir.join(html_p)
        } else if html_p.is_file() {
            html_p.clone()
        } else {
            return Err(Diagnostic::new(
                Phase::Semantic,
                format!(
                    "--html '{}': file not found (looked in '{}' and the working directory)",
                    html_p.display(),
                    if src_dir.as_os_str().is_empty() {
                        ".".to_string()
                    } else {
                        src_dir.display().to_string()
                    }
                ),
            ));
        };
        {
            let html_p = &resolved;
            let rel_path = resolved
                .strip_prefix(src_dir)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| PathBuf::from(resolved.file_name().unwrap_or_default()));

            let rel_parent = rel_path.parent().unwrap_or(Path::new(""));
            if !rel_parent.as_os_str().is_empty() && rel_parent != Path::new(".") {
                let actual_src_parent = html_p.parent().unwrap_or(Path::new("."));
                let dest_parent = out_dir.join(rel_parent);
                if actual_src_parent != dest_parent {
                    copy_dir_all(actual_src_parent, &dest_parent).map_err(|e| {
                        Diagnostic::new(
                            Phase::Semantic,
                            format!(
                                "failed to copy asset directory '{}': {}",
                                actual_src_parent.display(),
                                e
                            ),
                        )
                    })?;
                }
            } else {
                let file_name = html_p.file_name().unwrap_or_default();
                let dest_file = out_dir.join(file_name);
                if html_p != &dest_file {
                    let _ = fs::copy(html_p, &dest_file);
                }
                if let Some(src_parent) = opts.source_file.parent() {
                    let ui_dir = src_parent.join("ui");
                    let dest_ui = out_dir.join("ui");
                    if ui_dir.is_dir() && ui_dir != dest_ui {
                        let _ = copy_dir_all(&ui_dir, &dest_ui);
                    }
                }
            }
            html_arg = Some(rel_path.to_string_lossy().replace('\\', "/"));
        }
    }

    let is_gui = html_arg.is_some() || uses_gui_natives(program);

    // B4: `--self-contained` copies the runtime executable next to the app
    // and points the launcher at the local copy, so the bundle runs on a
    // clean machine — no global `spid3r` install required.
    let mut runtime_prefix = String::new();
    if opts.self_contained {
        let exe = std::env::current_exe().map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!("cannot locate the spid3r runtime to embed: {}", e),
            )
        })?;
        let runtime_name = if cfg!(target_os = "windows") {
            "spid3r.exe"
        } else {
            "spid3r"
        };
        fs::copy(&exe, out_dir.join(runtime_name)).map_err(|e| {
            Diagnostic::new(
                Phase::Semantic,
                format!(
                    "failed to copy runtime '{}' into the bundle: {}",
                    exe.display(),
                    e
                ),
            )
        })?;
        runtime_prefix = if cfg!(target_os = "windows") {
            ".\\".to_string()
        } else {
            "./".to_string()
        };
    }

    // 3. Write native platform launcher scripts
    if is_gui {
        let html_flag = if let Some(ref h) = html_arg {
            format!(" --html \"{}\"", h)
        } else {
            String::new()
        };
        #[cfg(target_os = "windows")]
        {
            // No `pause` for a GUI launcher: the console only exists to show
            // startup errors while the window runs, and holding it open for a
            // keypress after the window closes is noise. A console app keeps
            // it, so a double-clicked run.bat does not vanish with the output.
            let bat = format!(
                "@echo off\r\n{}spid3r gui \"{}.matrix\"{}\r\n",
                runtime_prefix, stem, html_flag
            );
            fs::write(out_dir.join("run.bat"), bat).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("failed to write run.bat: {}", e))
            })?;
        }
        #[cfg(not(target_os = "windows"))]
        {
            let sh = format!(
                "#!/bin/sh\n{}spid3r gui \"{}.matrix\"{}\n",
                runtime_prefix, stem, html_flag
            );
            let sh_path = out_dir.join("run.sh");
            fs::write(&sh_path, sh).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("failed to write run.sh: {}", e))
            })?;
        }
    } else {
        #[cfg(target_os = "windows")]
        {
            let bat = format!(
                "@echo off\r\n{}spid3r exec \"{}.matrixc\"\r\npause\r\n",
                runtime_prefix, stem
            );
            fs::write(out_dir.join("run.bat"), bat).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("failed to write run.bat: {}", e))
            })?;
        }
        #[cfg(not(target_os = "windows"))]
        {
            let sh = format!(
                "#!/bin/sh\n{}spid3r exec \"{}.matrixc\"\n",
                runtime_prefix, stem
            );
            let sh_path = out_dir.join("run.sh");
            fs::write(&sh_path, sh).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("failed to write run.sh: {}", e))
            })?;
        }
    }

    Ok(out_dir)
}

// ------------------------------------------------------------- ANDROID BUNDLER

fn bundle_android(opts: &BundleOptions, src: &str) -> Result<PathBuf, Diagnostic> {
    let stem = opts
        .source_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app");

    let raw_name = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect::<String>();
    let is_jvm_kw = matches!(
        raw_name.as_str(),
        "abstract"
            | "as"
            | "assert"
            | "boolean"
            | "break"
            | "byte"
            | "case"
            | "catch"
            | "char"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "do"
            | "double"
            | "else"
            | "enum"
            | "extends"
            | "false"
            | "final"
            | "finally"
            | "float"
            | "for"
            | "fun"
            | "goto"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "int"
            | "interface"
            | "is"
            | "long"
            | "native"
            | "new"
            | "null"
            | "object"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "short"
            | "static"
            | "strictfp"
            | "super"
            | "switch"
            | "synchronized"
            | "this"
            | "throw"
            | "throws"
            | "transient"
            | "true"
            | "try"
            | "typealias"
            | "typeof"
            | "val"
            | "var"
            | "void"
            | "volatile"
            | "when"
            | "while"
    );
    let app_name = if raw_name.is_empty()
        || is_jvm_kw
        || raw_name
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    {
        format!("app{}", raw_name)
    } else {
        raw_name
    };
    let out_dir = opts
        .output_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}_android", stem)));
    let assets_dir = out_dir.join("app").join("src").join("main").join("assets");
    let java_dir = out_dir
        .join("app")
        .join("src")
        .join("main")
        .join("java")
        .join("com")
        .join("spid3r")
        .join(&app_name);

    fs::create_dir_all(&assets_dir).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to create android assets dir: {}", e),
        )
    })?;
    fs::create_dir_all(&java_dir).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to create android java dir: {}", e),
        )
    })?;

    // Copy web assets to Android assets
    fs::write(assets_dir.join("spid3r.wasm"), EMBEDDED_WASM_BYTES).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write spid3r.wasm: {}", e),
        )
    })?;
    fs::write(assets_dir.join("spid3r.js"), EMBEDDED_JS_SDK).map_err(|e| {
        Diagnostic::new(Phase::Semantic, format!("failed to write spid3r.js: {}", e))
    })?;
    fs::write(assets_dir.join("app.matrix"), src).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write app.matrix: {}", e),
        )
    })?;

    // Index.html
    let index_html = r#"<!DOCTYPE html>
<html>
<head>
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <style>
    body { background: #0b0f17; color: #f1f5f9; font-family: sans-serif; padding: 16px; margin: 0; }
    h2 { color: #38bdf8; font-size: 1.25rem; }
    pre { background: #05080e; border: 1px solid rgba(255,255,255,0.1); border-radius: 8px; padding: 12px; font-family: monospace; white-space: pre-wrap; color: #38bdf8; }
  </style>
</head>
<body>
  <h2>⚡ Spid3r Android App</h2>
  <pre id="output">Loading Spid3r WebAssembly runtime...</pre>
  <script src="spid3r.js"></script>
  <script>
    (async function() {
      const out = document.getElementById('output');
      try {
        const sp = await Spid3r.init("spid3r.wasm");
        const resp = await fetch("app.matrix");
        const code = await resp.text();
        const res = sp.eval(code, "vm");
        out.innerText = res.stdout || (res.ok ? "Success (exit code " + res.exit_code + ")" : "Error: " + res.error);
      } catch (err) {
        out.innerText = "Fatal: " + err.message;
      }
    })();
  </script>
</body>
</html>"#;
    fs::write(assets_dir.join("index.html"), index_html).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write android index.html: {}", e),
        )
    })?;

    // AndroidManifest.xml
    let manifest = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="com.spid3r.{}">
    <uses-permission android:name="android.permission.INTERNET" />
    <application
        android:allowBackup="true"
        android:label="{}"
        android:theme="@android:style/Theme.NoTitleBar.Fullscreen">
        <activity
            android:name=".MainActivity"
            android:exported="true">
            <intent-filter>
                <action android:name="android.intent.action.MAIN" />
                <category android:name="android.intent.category.LAUNCHER" />
            </intent-filter>
        </activity>
    </application>
</manifest>"#,
        app_name,
        escape_markup(stem)
    );
    fs::write(
        out_dir
            .join("app")
            .join("src")
            .join("main")
            .join("AndroidManifest.xml"),
        manifest,
    )
    .map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write AndroidManifest.xml: {}", e),
        )
    })?;

    // MainActivity.kt
    let kt = format!(
        r#"package com.spid3r.{}

import android.annotation.SuppressLint
import android.app.Activity
import android.os.Bundle
import android.webkit.WebView
import android.webkit.WebViewClient

class MainActivity : Activity() {{
    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {{
        super.onCreate(savedInstanceState)
        val webView = WebView(this)
        webView.settings.javaScriptEnabled = true
        webView.settings.allowFileAccess = true
        webView.settings.domStorageEnabled = true
        webView.webViewClient = WebViewClient()
        webView.loadUrl("file:///android_asset/index.html")
        setContentView(webView)
    }}
}}"#,
        app_name
    );
    fs::write(java_dir.join("MainActivity.kt"), kt).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write MainActivity.kt: {}", e),
        )
    })?;

    // build.gradle.kts
    let gradle = format!(
        r#"plugins {{
    id("com.android.application") version "8.2.0"
    id("org.jetbrains.kotlin.android") version "1.9.20"
}}

android {{
    namespace = "com.spid3r.{app_name}"
    compileSdk = 34

    defaultConfig {{
        applicationId = "com.spid3r.{app_name}"
        minSdk = 24
        targetSdk = 34
        versionCode = 1
        versionName = "1.0"
    }}
}}
"#
    );
    fs::write(out_dir.join("app").join("build.gradle.kts"), gradle).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write build.gradle.kts: {}", e),
        )
    })?;

    Ok(out_dir)
}

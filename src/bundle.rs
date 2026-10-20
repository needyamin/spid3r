//! Application packager and cross-platform bundler for Spid3r.
//!
//! Supports bundling Spid3r scripts and applications for:
//! - WebAssembly / Browser (`--target wasm`): Standalone web directory or single-file HTML.
//! - Desktop (`--target desktop`): Precompiled `.matrixc` bytecode and desktop asset bundles.
//! - Mobile (`--target android`): Complete Android Studio WebView application project.

use std::fs;
use std::path::PathBuf;
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
        let out_html_path = opts
            .output_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("{}.html", stem)));
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
            stem,
            user_html_custom,
            // HTML end tags are case-insensitive and tolerate whitespace, so
            // a plain `</script>` replace is not enough to keep the program
            // source inside the <script type="application/spid3r"> element;
            // neutralize every `<` of a closing-tag sequence instead.
            src.replace("</", "<\\/"),
            EMBEDDED_JS_SDK,
            base64_wasm
        );

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
            stem
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

// ------------------------------------------------------------- DESKTOP BUNDLER

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

    // 2. If HTML frontend provided, copy it
    if let Some(ref html_p) = opts.html_file {
        if html_p.is_file() {
            let html_content = fs::read_to_string(html_p).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("failed to read HTML file: {}", e))
            })?;
            fs::write(out_dir.join("index.html"), html_content).map_err(|e| {
                Diagnostic::new(
                    Phase::Semantic,
                    format!("failed to write index.html: {}", e),
                )
            })?;
        }
    }

    // 3. Write native platform launcher scripts
    #[cfg(target_os = "windows")]
    {
        let bat = format!("@echo off\r\nspid3r exec \"{}.matrixc\"\r\npause\r\n", stem);
        fs::write(out_dir.join("run.bat"), bat).map_err(|e| {
            Diagnostic::new(Phase::Semantic, format!("failed to write run.bat: {}", e))
        })?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let sh = format!("#!/bin/sh\nspid3r exec \"{}.matrixc\"\n", stem);
        let sh_path = out_dir.join("run.sh");
        fs::write(&sh_path, sh).map_err(|e| {
            Diagnostic::new(Phase::Semantic, format!("failed to write run.sh: {}", e))
        })?;
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

    let app_name = stem
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>();
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
        app_name, stem
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
    let gradle = r#"plugins {
    id("com.android.application") version "8.2.0"
    id("org.jetbrains.kotlin.android") version "1.9.20"
}

android {
    namespace = "com.spid3r.app"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.spid3r.app"
        minSdk = 24
        targetSdk = 34
        versionCode = 1
        versionName = "1.0"
    }
}
"#;
    fs::write(out_dir.join("app").join("build.gradle.kts"), gradle).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("failed to write build.gradle.kts: {}", e),
        )
    })?;

    Ok(out_dir)
}

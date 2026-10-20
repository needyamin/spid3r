# 08 — Backends & Delivery Layer: native/LLVM, gui/wry, embed, bundle, wasm, build.rs

Static inspection, 2026-10-20. Scope: `src/native/*` (mod, llvm, ir, rt, link, modules), `src/gui/*` (mod, bridge, window), `src/embed.rs`, `src/bundle.rs`, `src/wasm.rs`, `build.rs`. No build/run performed (forbidden); all evidence is first-hand file reads at the cited lines. `Cargo.toml`, `web/spid3r.js`, `src/sched/mod.rs`, `src/interpreter/mod.rs`, `src/stdlib/mod.rs` were read as cross-reference evidence for contracts these files depend on. Siblings vm/, pkg/, stdlib/ are outside ownership except where a call crosses the boundary.

Confidence labels: **确定** = read directly this run; **高置信** = mechanism verified, trigger depends on generated/external input; **低置信** = plausible, not fully traceable within scope.

---

## 1. Ranked findings

### F1 — build.rs silently fetches a binary from the network and runs a nested release build on every plain `cargo build --release` (Windows, default features) — SEV high / conf 确定 on mechanism / med-high on real-world impact

- Trigger: `build.rs:41-45` — `windows_installer()` runs unless `SPID3R_NO_INSTALLER=1` (also skipped in nested builds, non-Windows, or when makensis is absent). Default features include `gui` (Cargo.toml `default = ["gui", "native"]`).
- Network: `build.rs:182-196` — `webview2_bootstrapper` downloads `https://go.microsoft.com/fwlink/p/?LinkId=2124703` via `Command::new("curl.exe").args(["-fsSL","--retry","2","-o"])` **with no checksum, size, or signature verification**. `usable()` is `p.metadata().len() > 0` (build.rs:185): curl can leave a **partial file after a mid-transfer failure** (no `--remove-on-error`), and a non-empty partial file is then cached as good and embedded into the shipped installer. The fwlink URL is a Microsoft-controlled *redirector* — the bytes are as trustworthy as the redirect target of the day.
- Cache location: `build.rs:186` — written into the **source tree** (`packaging/nsis/redist/`), mutating the repo (and, on this machine, OneDrive-syncing it).
- Nested build: `build.rs:114-131` — the installer path runs a second full `cargo build --release --bin spid3r --lib --no-default-features` into `target/installer`; a routine release build therefore compiles the world twice.
- Mitigations that exist: `SPID3R_NO_INSTALLER=1` (build.rs:42-45), or pre-seeding `packaging/nsis/redist/MicrosoftEdgeWebview2Setup.exe` (`usable()` accepts any existing non-empty file — which is also the integrity hole).
- Verdict: the supply-chain/surprise concern from the brief is **real**. Recommended: pin a SHA-256, use `--remove-on-error`/`--continue-at`, honor a "no network" env, move the cache out of the source tree.

### F2 — Native AOT runtime: cross-thread data race on non-atomic upvalue cells — SEV high (UB class) / conf 确定 mechanism, medium reachability

- `src/native/rt.rs:713` `pub struct UpvalueSlot { value: *mut Value, ... }`; `rt.rs:736-740` `spid3r_cell_set` does `unsafe { (*cell).value = v }` — a **plain non-atomic pointer write**; `rt.rs:728-734` `spid3r_cell_get` reads it.
- Cross-thread exposure: `rt.rs:1027` `unsafe impl Send for SendCfn {}` and `rt.rs:1034` `spid3r_spawn` run compiled closures on scheduler worker threads.
- Generator emits cell writes for captured-variable assignment: `src/native/ir.rs:1800-1808` (`self.tramp("spid3r_cell_set", ...)` for both local-cell and upvalue-cell paths).
- Consequence: `let c = ...; spawn(fn(){ c = 1 })` while the defining thread reads/writes `c` → unsynchronized concurrent access to a raw pointer slot = data race/UB. The Send justification (values are `Arc<Mutex>`-based) covers the *payloads*, not the cell slot itself. Bridge.rs solved the identical problem with poison-recovering locks (bridge.rs:47); rt.rs has no equivalent.
- Suggested fix direction: make `UpvalueSlot.value` an `AtomicPtr<Value>` (release/acquire) or a `Mutex`-wrapped slot; cost is one atomic op per upvalue access.

### F3 — AOT ABI has no unwind barrier; several raw-pointer contracts are unchecked, contradicting the module's own safety claim — SEV med-high / conf 确定

- Claim: `rt.rs:27-28` — "every dereference is defensively validated where a check is possible (`deref` and `cstr` null-check and abort cleanly...)".
- Violations of "where a check is possible":
  - `rt.rs:753-760` `spid3r_env_slot`: `*arr.add(index)` — **no bounds check** against the allocated count (`spid3r_env_alloc`, rt.rs:745-750, knows the count only as an opaque boxed slice). An ir.rs capture-index bug = OOB read of raw memory. Same for `spid3r_env_set_slot` (rt.rs:762-777).
  - `rt.rs:355-361` `args_vec`: `*argv.add(i)` with no `argv.is_null()` guard when `argc > 0` (called from spid3r_call/print/println/construct/method: rt.rs:377, 387, 396, 469, 487, 931, 937, 943).
  - `rt.rs:440-447` `spid3r_make_fn`, `rt.rs:845`, `rt.rs:858`: `std::mem::transmute(code)` with **no null check** on `code`.
  - `rt.rs:700-702` `spid3r_forin_end`: `drop(Box::from_raw(...))` — double-call from generated code = double free (no state invalidation).
- No catch_unwind anywhere at the extern "C" boundary: any panic inside runtime code — the `globals().lock().unwrap()` family (rt.rs:174, 323, 334, 342, 348, 792, 972; poisoned mutex) or a native/stdlib panic — unwinds into an `extern "C"` trampoline invoked from LLVM-generated frames, which **aborts the whole AOT program** with no diagnostic. Mitigating: `rt.rs:59` `abort()` gives clean messages for the *checked* paths, and error routing via `rt_throw`/THROWN covers expected runtime errors (rt.rs:262 `spid3r_binop` etc.).
- Verdict: today's generator is the only caller, so these are latent rather than active bugs — but the module's stated contract ("validated where a check is possible") is not met, and the failure mode for a violation is process abort or UB, not a catchable error.

### F4 — wasm32 target: silent behavioral divergences and unrecoverable panics — SEV med / conf 确定 on code, high on panic=abort property

- `src/sched/mod.rs:288-294` — `sleep_ms` is a **no-op** on wasm32 (`#[cfg(not(target_arch = "wasm32"))] thread::sleep` only): a script's `sleep(1000)` returns instantly, silently.
- `src/sched/mod.rs:296-310` — `now_ms()` returns **0** on wasm32: every time/timezone-ish API built on it reports epoch 0. No warning or error.
- `src/sched/mod.rs:251-271` — wasm32 `spawn_job` runs the job **inline** with `catch_unwind` (line 253). But wasm32-unknown-unknown's default panic strategy is abort: a Rust panic inside parser/interpreter during `spid3r_wasm_eval` (`src/wasm.rs:217-238`) **traps the entire instance**; the `"task panicked"` mapping is effectively unreachable. Every C-ABI entrypoint can trap on an internal panic — no isolation for the host page.
- Stack calibration: `src/interpreter/mod.rs:20` `MAX_CALL_DEPTH = 1_000` is documented (interpreter/mod.rs:18-23) as the guard that stops recursion "so this limit - not the OS stack size - is what stops runaway recursion", calibrated for 64 MB worker stacks (`RUN_STACK_BYTES`, interpreter/mod.rs:22-23). On wasm, `run_program_inline` runs on the **JS/browser stack (~1 MB)**: deep-but-legal recursion (≈1000 interpreted frames) can blow the wasm stack before the guard trips → hard trap instead of clean diagnostic. Conf med-high (per-frame native stack cost not measured here).
- Uncommitted cfg edits verdict: **plausible and needed.** wasm.rs deliberately calls `run_program_inline` (wasm.rs:96; embed.rs:222-224 documents the same rationale), which bypasses the gated big-stack thread (interpreter/mod.rs:187-191); `Vm::run_program`'s `go` path reaches `sched::spawn_job`, whose thread pool is gated at sched/mod.rs:14-16, 145-153, 166-167, 183-186, 260-283. `thread::Builder::spawn` compiles for wasm32 but spawning fails at runtime, so the gates are runtime-avoidance for the wasm path wasm.rs exercises. The edits look correct and minimal.

### F5 — wasm C-ABI: free contract is prefix-inclusive and only documented in Rust; non-UTF8 input silently becomes "" — SEV med (contract) / conf 确定

- Layout/ownership: `alloc_payload` (wasm.rs:182-191) returns `ptr` to a `[u8; 4-LE-len][json bytes]` buffer via `mem::forget`; `spid3r_wasm_free(ptr, len)` (wasm.rs:207-212) reconstructs `Vec::from_raw_parts(ptr, len, len)` — **the caller must pass the prefix-inclusive length (4 + payload)**. The JS SDK complies (web/spid3r.js:36-41 `free(resPtr, 4 + len)`; inputs freed at :58-59, :71, :83 with plain `len` after `spid3r_wasm_alloc(bytes.length)` at :24-30). A third-party consumer passing the *payload* length (the natural reading of "length-prefixed") triggers allocator UB. The rule lives in wasm.rs:197-201 doc text only — not in the JS SDK README a consumer would read first.
- Null/zero-length: handled (wasm.rs:219-233 etc. → `""` / `"tree"`). `spid3r_wasm_alloc(0)` returns a dangling-but-aligned pointer, freed as no-op (wasm.rs:208-210) — benign.
- Non-UTF8 source bytes: `std::str::from_utf8(...).unwrap_or("")` (wasm.rs:227, 246, 260; engine 232) — **silent data loss**: garbage input evaluates as an empty program and returns `ok:true`. A C-ABI error or explicit flag would be honest.
- Out-param overflow: N/A by design (no out-params; single returned buffer). A caller-supplied oversized `src_len` reads OOB of linear memory → wasm trap (containable), not host corruption.
- Minor: `json_escape` (wasm.rs:14-31) escapes control chars `< ' '` but not U+2028/U+2029 (break some JS embedding contexts) — low.

### F6 — bundle.rs Windows/launcher/Android edge cases — SEV low-med / conf 确定 (code), low-med (trigger frequency)

- `bundle.rs:307-309` — `run.bat` body `spid3r exec "{stem}.matrixc"`: relies on `spid3r` being on PATH (bundle ships no binary); `%` in a filename (legal on NTFS) is expanded by cmd and corrupts the command.
- `bundle.rs:317-322` — `run.sh` written without `+x` (no `PermissionsExt` usage anywhere in the file): not executable as shipped on Unix.
- `bundle.rs:338` — `app_name = stem.chars().filter(|c| c.is_alphanumeric())`: `char::is_alphanumeric` is Unicode-wide, so CJK stems produce invalid Java package segments (`package com.spid3r.你好` in MainActivity.kt, bundle.rs:447), and an all-punctuation stem produces an **empty** last package segment / `package "com.spid3r."` (bundle.rs:418-470 region). Android-only, build fails late.
- Unescaped interpolation: `<title>{}</title>` (bundle.rs:139, 226) and `android:label="{}"` (bundle.rs:404) take the raw stem — `<`/`"` in a filename (possible on Linux) breaks the HTML/XML. The script body itself *is* neutralized (`src.replace("</", "<\\/")`), so this is filename-only exposure.
- Path handling otherwise: `PathBuf::join` throughout, `.matrixc` quoted in launchers — sound on Windows apart from the `%` case.

### F7 — GUI layer robustness — SEV low-med / conf med-high

- `window.rs:191-199` (`file_url`): percent-encodes space, `"`, `<`, `>`, `#`, `?` but **not `%`** — a Windows path containing `%` (legal) yields a malformed `file://` URL and `--html page.html` fails to load. Should encode `%` first (`%25`).
- `window.rs:128-140` (`dispatch`): handlers run via `interp.call_value` **on the tao event-loop thread** (the main thread; default ~1 MB stack on Windows). The rest of the codebase deliberately runs scripts on 512 MB (embed.rs:206) / 64 MB (interpreter) workers. A gui_on handler doing deep recursion can hit a hard stack-overflow abort before `MAX_CALL_DEPTH` semantics matter. Conf med: per-frame cost unmeasured, but the asymmetry is real and contradicts embed's documented rationale.
- `window.rs:59-67`: `std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", ...)` — safe in edition 2021 (Cargo.toml:8) but `unsafe` in edition 2024; a latent migration blocker. LocalAppData→temp fallback is fine.
- Positive: `bridge.rs:47` recovers a poisoned mutex (`lock().unwrap_or_else(|p| p.into_inner())`), escape roundtrip tests at bridge.rs:222/321, lone-surrogate handling documented at bridge.rs:196. IPC body parsing is length-aware (take_out/take_events, bridge.rs:66-77).
- `window.rs:129` dispatch loop caps at 256 event batches per wake — leftover events stay queued for the next `Msg::Event`, not dropped (reasonable livelock guard).

### F8 — link.rs / native backend high-level version coupling — SEV low-med / conf 确定

- `link.rs:353` — Linux LLVM lib fallback list is `["-lLLVM-18", "-lLLVM-19", "-lLLVM"]`: silently accepts **LLVM 19** or any `libLLVM` while Cargo.toml pins `inkwell 0.10 / llvm18-1-no-llvm-linking` and build.rs warns "install LLVM 18.1+". The C API is mostly stable but this is an unguarded ABI drift (no version assert at runtime).
- `link.rs:257` vs `link.rs:375` — comment claims the PATH fallback excludes "Git's coreutils shim", but `which_program("link")` takes the **first** `link.exe` on PATH; on Git-heavy PATHs that is exactly the shim. Comment/code mismatch; the BuildTools scan (`HostX64/x64` only, link.rs:249, comment at :7) misses Community/Professional/Enterprise installs and ARM64 hosts.
- `link.rs:131` `default_triple` fallback builds `arch-unknown-os` (malformed triple layout) — fails late in `emit_object` with a confusing message.
- `llvm.rs` — the out-of-line `LLVM_InitializeAll*` shims (extern decls + `agg!` macro, `unsafe { $($fns();)+ }`) are a sound, well-documented workaround for LLVM ≥ 18's static-inline aggregates; NVPTX/XCore AsmParser omissions are explicitly noted (NB comment). Risk: if a future LLVM exports the aggregates, duplicate symbols at link time. `cpu_for_triple` hardcodes `arm64 → "apple-a14"` (arbitrary) — cosmetic.
- Native modules: `modules.rs` flattening has cycle detection and canonical-path dedup (flatten_stmts ~:27-56) — sound. Obj file written to temp with pid (link.rs:94); removal happens on the success path — a failed `build_native` leaves an orphan obj in temp (cosmetic).
- Memory strategy: `rt.rs:80` `leak()` — every Spid3r value is intentionally never freed (process-lifetime). Documented design; consequence worth restating: loop-heavy AOT programs grow RSS unboundedly (F-tier, design trade-off, not a bug).

### F9 — build.rs minor — `build.rs:82` `out_dir.ancestors().nth(4)` assumes the standard `target/{debug,release}/build/pkg-hash/out` layout; on `--target`-dir overrides the offset shifts — guarded only by a warning + installer skip (low).

---

## 2. Unsafe inventory (acceptance b) — every unsafe in owned files

**src/native/rt.rs** (the ABI surface; `#![allow(clippy::not_unsafe_ptr_arg_deref)]` at rt.rs:31 — safe-fn-with-raw-ptr design is deliberate and reasoned at rt.rs:25-29):
| Item | Line | Judgment |
|---|---|---|
| `fn globals()` static Mutex | rt.rs:52 | safe; `.lock().unwrap()` call sites (174/323/334/342/348/792/972) panic-poison risk → abort at ABI (F3) |
| `fn abort(msg) -> !` | rt.rs:59 | clean-exit design; sound |
| `fn deref(p) -> &Value` | rt.rs:66-72 | null-checked → abort; remaining deref is trusted-generator; sound-ish per contract |
| `fn leak(v) -> *mut Value` | rt.rs:80 | intentional leak; documented strategy |
| `spid3r_value_str` `slice::from_raw_parts` | rt.rs:222-227 | ptr/len from generated code; null deref guarded via `deref`; len trusted |
| `args_vec` (private `unsafe fn`) | rt.rs:355-361 | `*argv.add(i)`; **argv not null-checked when argc>0**; argc trusted (F3) |
| `spid3r_make_fn` transmute | rt.rs:440-447 | code ptr transmuted, **no null check** (F3) |
| `spid3r_forin_end` `Box::from_raw` | rt.rs:700-702 | double-call = double free; trusted (F3) |
| `UpvalueSlot` / `cell_get` / `cell_set` | rt.rs:713, 728-734, 736-740 | **non-atomic shared mutation** (F2) |
| `spid3r_env_alloc` boxed slice | rt.rs:745-750 | sound |
| `spid3r_env_slot` / `env_set_slot` | rt.rs:753-760, 762-777 | env+cell null-checked; **index not bounds-checked** (F3) |
| `class_register` / `class_mut` | rt.rs:781-805, 808-825 | `&mut *(class as *mut Value)` in a "registration window, not yet shared" argument, with `Arc::get_mut` → abort backstop; sound iff registration is single-threaded and pre-spawn — holds for current ir.rs startup emission |
| `class_add_method` / `set_ctor` transmutes | rt.rs:842-855, 857-870 | no null check on `code` (F3) |
| `unsafe impl Send for SendCfn` | rt.rs:1027 | justification covers Arc<Mutex> payloads, **not the cell slot** (F2) |
| tests with raw ctors | rt.rs:1257+ | test-only |

**src/wasm.rs** (6 unsafe fns): `alloc_payload` (182-191, safe fn doing `mem::forget` — ownership handed to caller), `spid3r_wasm_alloc` (194-199, same), `spid3r_wasm_free` (207-212, sound iff len matches allocation incl. prefix), `spid3r_wasm_eval` (217-238), `spid3r_wasm_check` (241-250), `spid3r_wasm_format` (255-264) — each guards null/zero-len; non-UTF8 → silent `""` (F5). Sound within the documented contract; contract under-documented for the prefix-inclusive free length.

**src/native/llvm.rs**: extern "C" declarations (~lines 13-160) — declarations only; `agg!` macro `unsafe { $($fns();)+ }` (~171-176) — calls resolve at link time (missing symbol = link error, not UB); native-target shims (~244-274). No derefs. Sound.

**src/native/ir.rs**: `argv_array` uses one `unsafe` in-bounds GEP with a known array type (bounds correct by construction); `FnTy` is an `unsafe extern "C" fn` alias (ABI mirror of rt.rs); `call_ptr`/`call_int` `panic!` on shape mismatch — compile-time crash, not UB. Entry-block alloca discipline (`alloca_entry`) prevents per-iteration stack growth — good. Sound.

**src/gui/window.rs, src/gui/mod.rs, src/gui/bridge.rs, src/embed.rs, src/bundle.rs, build.rs, src/native/link.rs, src/native/mod.rs, src/native/modules.rs**: **zero `unsafe` blocks.** window.rs:67 `set_var` is only unsafe-in-2024 (F7).

---

## 3. Focused answers

**(c) embed.rs panic poisoning**: A panic (e.g. a host-native `unwrap`) unwinds in the 512 MB worker (embed.rs:203-207); `take_capture` (embed.rs:225) never runs, so that run's captured output is lost; `join()` returns Err and embed.rs:241-243 re-raises via `std::panic::resume_unwind` — deliberately **not swallowed** (comment at :239-242). Net effect: the host's `engine.run()` call panics; a host without its own catch_unwind sees its process unwind/abort. No *silent* poisoning: output capture is thread-local (`stdlib/mod.rs:173-181`) and dies with the worker; spawn failure maps to `ScriptError` (embed.rs:232-240); the Err path carries partial output (test embed.rs:405). Residual: a panic while holding a stdlib process-global mutex (script args / globals shared by Value clones) could poison it for *subsequent* runs — not observed reachable in embed's own path.

**(d) wasm.rs C-ABI edge cases**: null/zero-len → defaults (guarded, sound); non-UTF8 → silent `""` (F5); no out-params to overflow (single length-prefixed return buffer); ownership: caller frees inputs, caller frees results with **prefix-inclusive** len — SDK correct (spid3r.js:41, 58-59), third parties can get it wrong → allocator UB (F5); `alloc(0)` dangling ptr benign; leaked buffers only if JS never frees (linear-memory growth).

**(e) native backend (high-level only)**: LLVM version coupling (F8: link.rs:353 accepts 19/unversioned vs 18.1 claim); no-unwind ABI + unchecked slots (F3); upvalue race (F2); class-in-fn limitation is explicitly enforced with a clear "run with --engine vm" error (ir.rs compile_stmt) — matches docs; leak-per-value memory strategy is a documented trade-off (rt.rs:80) with real long-run RSS consequences.

**(f) bundle.rs Windows**: PathBuf joins sound; `.matrixc` quoted; gaps are `%`-in-stem batch expansion, spid3r-on-PATH assumption (bundle.rs:307-309), no +x on run.sh (317-322), and the Android package-name derivation (338) (F6).

**(g) build.rs implicit network**: **Confirmed real** — see F1. Trigger surface: any Windows `cargo build --release` with default features, unless `SPID3R_NO_INSTALLER=1` or makensis is missing. Also a nested cargo release build (114-131). Integrity: unverified fwlink download, partial-file caching (185).

---

## 4. SOUND (explicitly checked and clean)

- wasm.rs null/zero-length guards and the JS SDK's alloc/free pairing (web/spid3r.js:24-30, 36-41, 58-59, 71, 83) — ownership is well-defined for SDK consumers.
- embed.rs: no unsafe at all; thread-local capture; partial output preserved on runtime errors (embed.rs:225, :405 test); panics re-raised, never swallowed.
- bridge.rs: poisoned-lock recovery (bridge.rs:47), escape roundtrip tests (:222, :321), surrogate handling documented (:196).
- native/modules.rs: import flattening with canonical-path dedup + cycle detection.
- ir.rs: entry-block alloca discipline; trusted-generator ABI mirrors rt.rs exactly (FnTy ↔ trampoline signatures); class-in-fn rejected with actionable error.
- llvm.rs: LLVM ≥ 18 initialize-shims are a correct, documented workaround; link-time (not UB) failure mode if symbols change.
- gui/event flow: events queued before window exists, replayed on ready; dispatch bounded (0..256) with leftovers retained.
- Cargo.toml: feature gating is clean (`native = ["dep:inkwell"]`, `gui = ["dep:wry","dep:tao"]`); core stays zero-dependency.

## 5. 待证项 (not fully verified this run)

- Reachability of F2's race in real programs: need an AOT test where a spawned task and the main thread touch the same captured var (would require building — forbidden here).
- Per-frame native stack cost of the tree interpreter (F4/F7 stack-margin math): measure via a deliberately deep recursion test on wasm and on the GUI event-loop thread.
- rt.rs line numbers cited as ranges (781-805, 808-825) were derived from a contiguous read window and are ±3 lines.
- The wasm32 build itself was not compiled (forbidden), so "cfg edits suffice" is a static-read conclusion, not a build result.

## 6. Next steps if more time

1. Hash-pin the WebView2 bootstrapper + `--remove-on-error` + honor a no-network env (F1, small diff, high value).
2. `AtomicPtr` for `UpvalueSlot.value` (F2, small diff, removes the UB class).
3. Bounds/assert in `env_slot` + `catch_unwind` shim at the ABI entry (F3), and an explicit wasm32 error for `now_ms`/`sleep_ms` instead of 0/no-op (F4).

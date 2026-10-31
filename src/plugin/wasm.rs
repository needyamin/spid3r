//! The WASM plugin host: loads a `.wasm` module and exposes its exports as
//! script-callable functions.
//!
//! This is the **only genuinely sandboxed** plugin kind in Spid3r. A guest
//! module has no ambient authority: it cannot open a file, spawn a process, read
//! an environment variable or reach the network, because the host links no such
//! imports into it. Everything it can do arrives as an argument and leaves as a
//! return value, through [`super::abi`].
//!
//! That is a different claim from the capability filter. `Engine::deny` stops a
//! *script* from calling a function that exists; this stops the *function* from
//! existing in the first place. Both are real, and only one of them is a
//! sandbox.
//!
//! # The guest contract
//!
//! A module must export:
//!
//! | export | signature | purpose |
//! | --- | --- | --- |
//! | `memory` | — | the linear memory the host reads and writes |
//! | `alloc` | `(i32) -> i32` | reserve `len` bytes, return the pointer |
//! | one per function | `(i32, i32) -> i64` | take `(ptr, len)`, return `(ptr << 32) \| len` |
//!
//! `dealloc` is optional and called when exported. A function's export name is
//! its manifest name, so there is no dispatch table inside the guest.
//!
//! # Budget
//!
//! Fuel is consumed per instruction. A plugin call is given the host's remaining
//! budget when one is set, so a runaway guest is terminated instead of hanging
//! the interpreter. This is the one place a budget is enforceable *below* the
//! language, and it is why a WASM plugin is safe to run untrusted.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use wasmi::{Config, Engine, Linker, Memory, Module, Store, TypedFunc};

use crate::value::{NativeClosureFn, NativeCtx, Value};

use super::abi;
use super::manifest::{PluginKind, PluginManifest, ABI_V1};

/// Fuel granted to one plugin call when the host set no budget.
///
/// Generous enough for real work (tens of millions of instructions) and small
/// enough that a `loop {}` is stopped in well under a second.
pub const DEFAULT_FUEL: u64 = 50_000_000;

/// A live guest instance. Held behind a mutex because a `Store` is not `Sync`.
struct Guest {
    store: Store<()>,
    memory: Memory,
    /// Export name → typed function. Looked up once at load, not per call.
    funcs: HashMap<String, TypedFunc<(u32, u32), i64>>,
    dealloc: Option<TypedFunc<(u32, u32), ()>>,
    alloc: TypedFunc<u32, u32>,
    fuel: Option<u64>,
}

/// A loaded WASM plugin: its manifest and the closures that call into it.
pub struct LoadedWasm {
    pub manifest: PluginManifest,
    pub closures: Vec<(String, Arc<NativeClosureFn>)>,
    /// Kept alive for the process; dropping it tears the instance down.
    _guest: Arc<Mutex<Guest>>,
}

impl std::fmt::Debug for LoadedWasm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The guest is opaque by design (a `Store` is not `Debug`), so report
        // what a caller can act on rather than hiding the type entirely.
        f.debug_struct("LoadedWasm")
            .field("name", &self.manifest.name)
            .field("version", &self.manifest.version)
            .field("functions", &self.closures.len())
            .finish_non_exhaustive()
    }
}

/// Load `plugin.toml` + its `entry` module from `dir`.
///
/// Rejects a manifest whose `kind` is not `wasm`, and one whose `entry` does not
/// exist, before compiling anything.
pub fn load(dir: &Path) -> Result<LoadedWasm, String> {
    let manifest = PluginManifest::load(dir)?;
    if manifest.kind != PluginKind::Wasm {
        return Err(format!(
            "plugin '{}' is kind = \"{}\"; the WASM loader only accepts \"wasm\"",
            manifest.name,
            manifest.kind.as_str()
        ));
    }
    if !manifest.abi_matches() {
        return Err(format!(
            "plugin '{}' targets ABI '{}' but this build implements '{}'",
            manifest.name, manifest.abi, ABI_V1
        ));
    }
    if manifest.entry.is_empty() {
        return Err(format!(
            "plugin '{}' has kind = \"wasm\" but no `entry` (the .wasm file name)",
            manifest.name
        ));
    }
    let wasm_path = dir.join(&manifest.entry);
    let bytes = std::fs::read(&wasm_path)
        .map_err(|e| format!("cannot read {}: {}", wasm_path.display(), e))?;

    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &bytes[..])
        .map_err(|e| format!("plugin '{}': not a valid WASM module: {}", manifest.name, e))?;

    let mut store: Store<()> = Store::new(&engine, ());
    store.set_fuel(DEFAULT_FUEL).map_err(|e| e.to_string())?;
    // An empty linker: the module may import nothing. A module that imports
    // WASI — `fd_write`, `clock_time_get`, … — fails to instantiate here, which
    // is exactly the sandbox working as intended rather than an oversight.
    let linker: Linker<()> = Linker::new(&engine);
    let instance_pre = linker.instantiate(&mut store, &module).map_err(|e| {
        format!(
            "plugin '{}': cannot instantiate (it must not import host functions): {}",
            manifest.name, e
        )
    })?;
    // `start` runs the module's start section, if it has one. It draws from the
    // same fuel budget as any call, so a start section that loops is still
    // bounded.
    let instance = instance_pre.start(&mut store).map_err(|e| {
        format!(
            "plugin '{}': the start function failed: {}",
            manifest.name, e
        )
    })?;

    let memory = instance.get_memory(&store, "memory").ok_or_else(|| {
        format!(
            "plugin '{}': no `memory` export — a WASM plugin must export its linear memory",
            manifest.name
        )
    })?;
    let alloc = instance
        .get_typed_func::<u32, u32>(&store, "alloc")
        .map_err(|e| {
            format!(
                "plugin '{}': missing `alloc(i32) -> i32`: {}",
                manifest.name, e
            )
        })?;
    let dealloc = instance
        .get_typed_func::<(u32, u32), ()>(&store, "dealloc")
        .ok();

    let mut funcs = HashMap::new();
    for decl in &manifest.functions {
        let f = instance
            .get_typed_func::<(u32, u32), i64>(&store, decl.name.as_str())
            .map_err(|e| {
                format!(
                    "plugin '{}': manifest declares '{}' but the module does not export it as (i32, i32) -> i64: {}",
                    manifest.name, decl.name, e
                )
            })?;
        funcs.insert(decl.name.clone(), f);
    }

    let guest = Arc::new(Mutex::new(Guest {
        store,
        memory,
        funcs,
        dealloc,
        alloc,
        fuel: None,
    }));

    let mut closures: Vec<(String, Arc<NativeClosureFn>)> = Vec::new();
    for decl in &manifest.functions {
        let g = guest.clone();
        let export = decl.name.clone();
        let closure: Arc<NativeClosureFn> =
            Arc::new(move |_ctx: &NativeCtx, args: Vec<Value>| call(&g, &export, args));
        closures.push((decl.name.clone(), closure));
    }

    Ok(LoadedWasm {
        manifest,
        closures,
        _guest: guest,
    })
}

fn lock(guest: &Arc<Mutex<Guest>>) -> MutexGuard<'_, Guest> {
    // A guest that panics must not poison the plugin for the rest of the run:
    // the fuel limit means a well-formed module cannot, but a bug in this file
    // should not take the interpreter with it.
    guest.lock().unwrap_or_else(|e| e.into_inner())
}

/// Call one exported function: encode → write → invoke → read → decode.
fn call(guest: &Arc<Mutex<Guest>>, export: &str, args: Vec<Value>) -> Result<Value, String> {
    let req = abi::encode_args(&args)?;
    let mut g = lock(guest);

    // `TypedFunc` and `Memory` are `Copy` handles, so take them out before any
    // mutable borrow of the store: calling `g.alloc.call(&mut g.store, …)`
    // directly is a double borrow.
    let (alloc, memory, dealloc, func) = {
        let func = *g
            .funcs
            .get(export)
            .ok_or_else(|| format!("plugin: no export named '{export}'"))?;
        (g.alloc, g.memory, g.dealloc, func)
    };

    let fuel = g.fuel.unwrap_or(DEFAULT_FUEL);
    g.store
        .set_fuel(fuel)
        .map_err(|e| format!("cannot set plugin fuel: {e}"))?;

    let len = u32::try_from(req.len()).map_err(|_| "arguments too large".to_string())?;
    let ptr = alloc
        .call(&mut g.store, len)
        .map_err(|e| format!("plugin '{export}': alloc failed: {e}"))?;

    // The allocation must land inside the current memory, or the write below
    // would be a bounds violation dressed up as a host bug.
    let mem_len = memory.data_size(&g.store) as u64;
    if u64::from(ptr) + u64::from(len) > mem_len {
        return Err(format!(
            "plugin '{export}': alloc returned {ptr} for {len} byte(s), outside {mem_len} byte(s) of memory"
        ));
    }
    memory
        .data_mut(&mut g.store)
        .get_mut(ptr as usize..(ptr as usize + len as usize))
        .ok_or_else(|| format!("plugin '{export}': alloc returned an out-of-bounds pointer"))?
        .copy_from_slice(&req);

    let packed = func.call(&mut g.store, (ptr, len)).map_err(|e| {
        // Fuel exhaustion arrives here as a trap.
        format!("plugin '{export}' failed: {e}")
    })?;

    let out_ptr = (packed as u64 >> 32) as u32;
    let out_len = (packed as u64 & 0xFFFF_FFFF) as u32;
    if out_len as usize > abi::MAX_PAYLOAD {
        return Err(format!(
            "plugin '{export}' returned {out_len} byte(s), over the {} byte limit",
            abi::MAX_PAYLOAD
        ));
    }
    // Re-read the size: the guest may have grown memory during the call.
    let mem_len = memory.data_size(&g.store) as u64;
    if u64::from(out_ptr) + u64::from(out_len) > mem_len {
        return Err(format!(
            "plugin '{export}': returned a pointer outside its memory ({out_ptr} + {out_len} > {mem_len})"
        ));
    }
    let bytes =
        memory.data(&g.store)[out_ptr as usize..(out_ptr as usize + out_len as usize)].to_vec();

    // Release the request buffer if the guest offers to.
    if let Some(dealloc) = dealloc {
        let _ = dealloc.call(&mut g.store, (ptr, len));
    }

    abi::decode_response(&bytes)
}

/// Set the per-call fuel for a loaded plugin. Used by a host that wants a
/// tighter budget than [`DEFAULT_FUEL`].
pub fn set_fuel(loaded: &mut LoadedWasm, fuel: u64) {
    // The closures hold their own handle; this reaches the same instance.
    if let Ok(mut g) = loaded._guest.lock() {
        g.fuel = Some(fuel);
    }
}

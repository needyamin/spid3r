/**
 * Spid3r WebAssembly JavaScript SDK
 * Zero-dependency client-side execution engine for browsers and Node.js.
 */
(function(global) {
  'use strict';

  class Spid3rRuntime {
    constructor(wasmInstance) {
      this.instance = wasmInstance;
      this.exports = wasmInstance.exports;
      this.memory = this.exports.memory;
      this.encoder = new TextEncoder();
      this.decoder = new TextDecoder('utf-8');
    }

    /**
     * Allocate a string buffer in WASM linear memory.
     * @private
     */
    _writeString(str) {
      const bytes = this.encoder.encode(str);
      const ptr = this.exports.spid3r_wasm_alloc(bytes.length);
      const view = new Uint8Array(this.memory.buffer, ptr, bytes.length);
      view.set(bytes);
      return { ptr, len: bytes.length };
    }

    /**
     * Read a length-prefixed JSON buffer from WASM linear memory and free it.
     * @private
     */
    _readPayload(resPtr) {
      if (!resPtr) return { ok: false, error: "Null pointer returned from WASM" };
      try {
        const view = new DataView(this.memory.buffer);
        const len = view.getUint32(resPtr, true); // little-endian
        const bytes = new Uint8Array(this.memory.buffer, resPtr + 4, len);
        const jsonStr = this.decoder.decode(bytes);
        // Free the buffer (4 bytes length header + string bytes)
        this.exports.spid3r_wasm_free(resPtr, 4 + len);
        return JSON.parse(jsonStr);
      } catch (err) {
        return { ok: false, error: "Failed to parse WASM response: " + err.message };
      }
    }

    /**
     * Run Spid3r source code using either the bytecode VM ("vm") or AST interpreter ("tree").
     * @param {string} source - Spid3r source code to execute.
     * @param {string} [engine="vm"] - Execution engine: "vm" (default) or "tree".
     * @returns {{ ok: boolean, exit_code?: number, stdout?: string, error?: string, thrown?: string, line?: number, col?: number }}
     */
    eval(source, engine = "vm") {
      if (typeof this.exports.spid3r_wasm_set_epoch_ms === 'function') {
        this.exports.spid3r_wasm_set_epoch_ms(Date.now());
      }
      const s = this._writeString(source || "");
      const e = this._writeString(engine || "vm");
      const resPtr = this.exports.spid3r_wasm_eval(s.ptr, s.len, e.ptr, e.len);
      this.exports.spid3r_wasm_free(s.ptr, s.len);
      this.exports.spid3r_wasm_free(e.ptr, e.len);
      return this._readPayload(resPtr);
    }

    /**
     * Lex, parse, and semantic check source code without executing.
     * @param {string} source - Spid3r source code.
     * @returns {{ ok: boolean, phase?: string, error?: string, line?: number, col?: number }}
     */
    check(source) {
      const s = this._writeString(source || "");
      const resPtr = this.exports.spid3r_wasm_check(s.ptr, s.len);
      this.exports.spid3r_wasm_free(s.ptr, s.len);
      return this._readPayload(resPtr);
    }

    /**
     * Formats Spid3r source code according to official canonical style.
     * @param {string} source - Spid3r source code to format.
     * @returns {{ ok: boolean, code?: string, error?: string }}
     */
    format(source) {
      const s = this._writeString(source || "");
      const resPtr = this.exports.spid3r_wasm_format(s.ptr, s.len);
      this.exports.spid3r_wasm_free(s.ptr, s.len);
      return this._readPayload(resPtr);
    }

    /**
     * Retrieves engine and language version information.
     * @returns {{ version: string, language_version: string }}
     */
    version() {
      const resPtr = this.exports.spid3r_wasm_version();
      return this._readPayload(resPtr);
    }

    /**
     * Register an in-memory module source so `import "<name>"` resolves inside WASM.
     * Note: desktop `gui_*` (`tao`/`wry`) natives are desktop-only and are not part of browser WASM.
     * @param {string} name
     * @param {string} source
     */
    registerModule(name, source) {
      const n = this._writeString(name || "");
      const s = this._writeString(source || "");
      const resPtr = this.exports.spid3r_wasm_register_module(n.ptr, n.len, s.ptr, s.len);
      this.exports.spid3r_wasm_free(n.ptr, n.len);
      this.exports.spid3r_wasm_free(s.ptr, s.len);
      return this._readPayload(resPtr);
    }

    /**
     * Clear all registered in-memory virtual modules.
     */
    clearModules() {
      const resPtr = this.exports.spid3r_wasm_clear_modules();
      return this._readPayload(resPtr);
    }

    /**
     * Evaluate Spid3r source in a persistent session (stateful across calls).
     * @param {string} source
     * @param {string} [engine="vm"]
     */
    evalSession(source, engine = "vm") {
      if (typeof this.exports.spid3r_wasm_set_epoch_ms === 'function') {
        this.exports.spid3r_wasm_set_epoch_ms(Date.now());
      }
      const s = this._writeString(source || "");
      const e = this._writeString(engine || "vm");
      const resPtr = this.exports.spid3r_wasm_session_eval(s.ptr, s.len, e.ptr, e.len);
      this.exports.spid3r_wasm_free(s.ptr, s.len);
      this.exports.spid3r_wasm_free(e.ptr, e.len);
      return this._readPayload(resPtr);
    }

    /**
     * Reset the persistent WASM session state.
     */
    resetSession() {
      const resPtr = this.exports.spid3r_wasm_session_reset();
      return this._readPayload(resPtr);
    }
  }

  /**
   * Main SDK Loader
   */
  const Spid3r = {
    /**
     * Synchronously initialize the Spid3r WebAssembly runtime from an ArrayBuffer, TypedArray, or WebAssembly.Module.
     * @param {BufferSource|WebAssembly.Module} source
     * @returns {Spid3rRuntime}
     */
    initSync(source) {
      if (source instanceof WebAssembly.Module) {
        const instance = new WebAssembly.Instance(source, {});
        return new Spid3rRuntime(instance);
      }
      if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) {
        const module = new WebAssembly.Module(source);
        const instance = new WebAssembly.Instance(module, {});
        return new Spid3rRuntime(instance);
      }
      throw new Error("initSync requires an ArrayBuffer, TypedArray, or WebAssembly.Module");
    },

    /**
     * Initialize the Spid3r WebAssembly runtime.
     * @param {string|URL|Response|BufferSource|WebAssembly.Module} [source="web/spid3r.wasm"]
     * @returns {Promise<Spid3rRuntime>}
     */
    async init(source = "web/spid3r.wasm") {
      let moduleOrInstance;

      if (source instanceof WebAssembly.Module) {
        try {
          const instance = new WebAssembly.Instance(source, {});
          return new Spid3rRuntime(instance);
        } catch (_) {
          const instance = await WebAssembly.instantiate(source, {});
          return new Spid3rRuntime(instance);
        }
      }

      if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) {
        try {
          const module = new WebAssembly.Module(source);
          const instance = new WebAssembly.Instance(module, {});
          return new Spid3rRuntime(instance);
        } catch (_) {
          const { instance } = await WebAssembly.instantiate(source, {});
          return new Spid3rRuntime(instance);
        }
      }

      if (typeof Response !== 'undefined' && source instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
          try {
            const { instance } = await WebAssembly.instantiateStreaming(source.clone(), {});
            return new Spid3rRuntime(instance);
          } catch (_) {
            // Fallback to arrayBuffer if streaming instantiation fails (e.g. non-wasm MIME type)
          }
        }
        const bytes = await source.arrayBuffer();
        return this.init(bytes);
      }

      if (typeof source === 'string' || (typeof URL !== 'undefined' && source instanceof URL)) {
        if (typeof fetch === 'function' && typeof WebAssembly.instantiateStreaming === 'function') {
          try {
            const response = await fetch(source);
            const { instance } = await WebAssembly.instantiateStreaming(response, {});
            return new Spid3rRuntime(instance);
          } catch (_) {
            // Fallback to arrayBuffer if streaming instantiation fails (e.g. file:// or mime type)
          }
        }
        const resp = await fetch(source);
        const bytes = await resp.arrayBuffer();
        return this.init(bytes);
      }

      throw new Error("Unsupported WebAssembly source provided to Spid3r.init()");
    }
  };

  // Export for browser globals and module environments
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = Spid3r;
  }
  if (typeof window !== 'undefined') {
    window.Spid3r = Spid3r;
  }
  global.Spid3r = Spid3r;
})(typeof globalThis !== 'undefined' ? globalThis : this);

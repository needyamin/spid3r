// Shared dynamic version resolver for all docs/html/*.html pages.
// Source priority:
//   1. Live WASM runtime (`runtime.version()` in playground.html / doc.html)
//   2. Live `../../Cargo.toml` + `../../src/lib.rs` when served over http(s)
//   3. Built-in fallback matching the current repository release (works on file://)
(function (g) {
  'use strict';

  var current = {
    version: '1.5.2',
    language_version: '1.5.2'
  };
  var authoritative = false;

  function formatVersion(kind, info) {
    var v = (info && info.version) || current.version;
    var lang = (info && info.language_version) || current.language_version || v;
    switch (kind) {
      case 'v':       return 'v' + v;
      case 'wasm':    return 'WASM ' + v;
      case 'release': return 'v' + v + ' Release';
      case 'cli':     return 'spid3r ' + v + ' (language ' + lang + ')';
      case 'plain':   return v;
      case 'lang':    return lang;
      case 'full':    return 'v' + v + ' (language ' + lang + ')';
      default:        return 'v' + v;
    }
  }

  function applyToDom(info) {
    if (typeof document === 'undefined') return;
    var nodes = document.querySelectorAll('[data-spid3r-version]');
    for (var i = 0; i < nodes.length; i++) {
      var el = nodes[i];
      var kind = el.getAttribute('data-spid3r-version') || 'v';
      el.textContent = formatVersion(kind, info);
    }
  }

  function spid3rApplyVersion(info, opts) {
    if (info && typeof info === 'object') {
      if (info.version) current.version = String(info.version);
      if (info.language_version) current.language_version = String(info.language_version);
    }
    if (opts && opts.authoritative) {
      authoritative = true;
    }
    g.SPID3R_VERSION = {
      version: current.version,
      language_version: current.language_version
    };
    applyToDom(current);
    return g.SPID3R_VERSION;
  }

  function fetchRepoVersion() {
    if (authoritative) return;
    if (typeof fetch !== 'function' || typeof location === 'undefined') return;
    if (location.protocol !== 'http:' && location.protocol !== 'https:') return;

    fetch('../../Cargo.toml', { cache: 'no-cache' })
      .then(function (r) { return r.ok ? r.text() : null; })
      .then(function (toml) {
        if (!toml || authoritative) return;
        var m = toml.match(/^\s*version\s*=\s*"([^"]+)"/m);
        if (m && m[1]) {
          spid3rApplyVersion({ version: m[1] });
        }
      })
      .catch(function () { /* offline or not served from repo root */ });

    fetch('../../src/lib.rs', { cache: 'no-cache' })
      .then(function (r) { return r.ok ? r.text() : null; })
      .then(function (src) {
        if (!src || authoritative) return;
        var m = src.match(/LANGUAGE_VERSION\s*:\s*&str\s*=\s*"([^"]+)"/);
        if (m && m[1]) {
          spid3rApplyVersion({ language_version: m[1] });
        }
      })
      .catch(function () { /* offline or not served from repo root */ });
  }

  g.SPID3R_VERSION = {
    version: current.version,
    language_version: current.language_version
  };
  g.spid3rApplyVersion = spid3rApplyVersion;

  if (typeof document !== 'undefined') {
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', function () {
        applyToDom(current);
        fetchRepoVersion();
      });
    } else {
      applyToDom(current);
      fetchRepoVersion();
    }
  }
})(typeof window !== 'undefined' ? window : (typeof globalThis !== 'undefined' ? globalThis : this));

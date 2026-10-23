// Exercise the playground's ACTUAL load path. docs/html/playground.html loads
// its LOCAL siblings with relative <script> tags — docs/html/spid3r.js and
// docs/html/spid3r_wasm_data.js — not the web/ copies, and it prefers the
// embedded base64 module over fetching a .wasm file. So the docs/html copies
// are what visitors actually run. The other wasm tools read
// web/spid3r.wasm directly and would pass while the playground ships a stale
// module (which it once did: the docs/html copy sat five rebuilds behind and
// its stdlib preset could not import anything).
//
// This harness mirrors initEngine() for both deployment copies:
//   A. docs/html/*  — the playground path (primary): every preset, both engines
//   B. web/*        — the SDK distribution path: version + stdlib preset
// Both embedded copies must equal web/spid3r.wasm byte for byte.
// Run: node tools/test_wasm_embedded.js
const fs = require('fs');
const path = require('path');
const assert = require('assert');

const root = path.join(__dirname, '..');

// Pull the presets out of the playground page so this tracks the real UI.
const html = fs.readFileSync(path.join(root, 'docs', 'html', 'playground.html'), 'utf8');
const start = html.indexOf('const PRESETS = {');
const endMarker = '\n      };';
const end = html.indexOf(endMarker, start);
assert.ok(start >= 0 && end >= 0, 'could not locate PRESETS in playground.html');
const literal = html
  .slice(start + 'const PRESETS = '.length, end + endMarker.length)
  .replace(/;\s*$/, '');
const PRESETS = eval('(' + literal + ')');

const onDisk = fs.readFileSync(path.join(root, 'web', 'spid3r.wasm'));

// decodeBase64ToBytes(), the playground's own conversion (atob equivalent)
function decodeBase64ToBytes(b64) {
  const binStr = Buffer.from(b64, 'base64').toString('binary');
  const bytes = new Uint8Array(binStr.length);
  for (let i = 0; i < binStr.length; i++) bytes[i] = binStr.charCodeAt(i);
  return bytes;
}

// Load one deployment copy the way the browser does and return a live runtime.
function loadDeployment(dir) {
  const Spid3r = require(path.join(root, dir, 'spid3r.js'));
  require(path.join(root, dir, 'spid3r_wasm_data.js'));
  const b64 = globalThis.__SPID3R_WASM_BASE64__;
  assert.ok(typeof b64 === 'string' && b64.length > 0, dir + '/spid3r_wasm_data.js did not export __SPID3R_WASM_BASE64__');
  const bytes = decodeBase64ToBytes(b64);
  assert.strictEqual(bytes.length, onDisk.length, dir + ': embedded wasm length differs from web/spid3r.wasm');
  assert.ok(Buffer.compare(Buffer.from(bytes), onDisk) === 0,
    dir + ': embedded wasm is STALE — regenerate with: node tools/gen_wasm_data.js');
  // initSync is what the playground calls for a TypedArray input.
  return Spid3r.initSync(bytes);
}

let failures = 0;

// A. The playground path: every preset, both engines, plus divergence check.
const runtime = loadDeployment(path.join('docs', 'html'));
const ver = runtime.version();
console.log('playground path version:', JSON.stringify(ver));

for (const [name, source] of Object.entries(PRESETS)) {
  const vm = runtime.eval(source, 'vm');
  const tree = runtime.eval(source, 'tree');
  if (!vm.ok) {
    console.error(`FAIL ${name} [vm]: ${vm.phase} ${vm.error} (line ${vm.line})`);
    failures++;
  }
  if (!tree.ok) {
    console.error(`FAIL ${name} [tree]: ${tree.phase} ${tree.error} (line ${tree.line})`);
    failures++;
  }
  if (vm.ok && tree.ok && vm.stdout !== tree.stdout) {
    console.error(`DIVERGENCE ${name}: vm and tree disagree`);
    failures++;
  }
  console.log(`ok ${name}`);
}

// The stdlib preset is the one that proves the compiled-in stdlib works
// through the embedded path (import + gzip/sha256/toml natives).
const std = runtime.eval(PRESETS.stdlib, 'vm');
if (!std.ok) {
  console.error('FAIL stdlib preset:', JSON.stringify(std));
  failures++;
} else {
  for (const needle of ['name: spid3r', '1.2.3 vs 1.10.0: -1', 'sha256(abc): ba7816bf', 'round trip ok: true']) {
    if (!std.stdout.includes(needle)) {
      console.error('missing expected stdlib output:', needle);
      failures++;
    }
  }
}

// B. The SDK distribution path: same module, same presets must hold.
const sdk = loadDeployment('web');
const sdkStd = sdk.eval(PRESETS.stdlib, 'vm');
if (!sdkStd.ok || sdkStd.stdout !== std.stdout) {
  console.error('FAIL: web/ embedded copy disagrees with the playground copy');
  failures++;
} else {
  console.log('ok web/ embedded copy matches the playground copy');
}

if (failures) process.exit(1);
console.log(`\nembedded wasm OK: ${Object.keys(PRESETS).length} presets on both engines, both deployment copies current`);

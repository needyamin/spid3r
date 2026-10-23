// Exercise the playground's ACTUAL load path. docs/html/playground.html loads
// its LOCAL siblings (docs/html/spid3r.js + docs/html/spid3r_wasm_data.js)
// via relative <script> tags and prefers the embedded base64 module over
// fetching a .wasm file, so the docs/html copies are what visitors actually
// run. The other wasm tools read web/spid3r.wasm directly and would pass
// while the playground ships a stale module (which it once did: the
// docs/html copy sat five rebuilds behind and its stdlib preset could not
// import anything; the web/ copy sat three behind).
//
// Two modes:
//   node tools/test_wasm_embedded.js                      (self-consistency)
//     Both embedded copies must equal web/spid3r.wasm byte for byte, every
//     preset must run on both engines with no divergence, and the stdlib
//     natives must work. Run against the committed tree.
//
//   node tools/test_wasm_embedded.js --against <wasm>     (behavioral)
//     Same preset suite through the committed embedded module, plus every
//     preset's stdout must match a freshly built reference module exactly.
//     CI uses this: wasm is not byte-reproducible across machines, so the
//     reference is compared behaviorally, never byte-wise. A committed
//     module old enough to lack the embedded stdlib fails the stdlib preset
//     outright; any other behavioral drift fails the stdout comparison.
//
// Run: node tools/test_wasm_embedded.js [--against web/spid3r.wasm]
const fs = require('fs');
const path = require('path');
const assert = require('assert');

const root = path.join(__dirname, '..');

const args = process.argv.slice(2);
const againstIdx = args.indexOf('--against');
const referencePath = againstIdx >= 0 ? path.resolve(args[againstIdx + 1]) : null;
if (againstIdx >= 0 && !referencePath) {
  console.error('--against requires a path to a freshly built spid3r_lang.wasm');
  process.exit(1);
}

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
  if (!referencePath) {
    // Self-consistency mode only: the embedded copy must be the deployed
    // binary, byte for byte. (Skipped in --against mode, where the reference
    // is a different machine's build and is compared behaviorally below.)
    assert.strictEqual(bytes.length, onDisk.length, dir + ': embedded wasm length differs from web/spid3r.wasm');
    assert.ok(Buffer.compare(Buffer.from(bytes), onDisk) === 0,
      dir + ': embedded wasm is STALE — regenerate with: node tools/gen_wasm_data.js');
  }
  // initSync is what the playground calls for a TypedArray input.
  return Spid3r.initSync(bytes);
}

let failures = 0;

// A. The playground path: every preset, both engines, plus divergence check.
const runtime = loadDeployment(path.join('docs', 'html'));
const ver = runtime.version();
console.log('playground path version:', JSON.stringify(ver));

const outputs = {};
for (const [name, source] of Object.entries(PRESETS)) {
  const vm = runtime.eval(source, 'vm');
  const tree = runtime.eval(source, 'tree');
  outputs[name] = { vm: vm.ok ? vm.stdout : null, tree: tree.ok ? tree.stdout : null };
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
// through the embedded path (import + gzip/sha256/toml natives). A module
// built before the stdlib was embedded fails here with an import error.
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

// B. The SDK distribution path: same module, same outputs must hold.
const sdk = loadDeployment('web');
const sdkStd = sdk.eval(PRESETS.stdlib, 'vm');
if (!sdkStd.ok || sdkStd.stdout !== std.stdout) {
  console.error('FAIL: web/ embedded copy disagrees with the playground copy');
  failures++;
} else {
  console.log('ok web/ embedded copy matches the playground copy');
}

// C. Behavioral comparison against a freshly built reference module.
if (referencePath) {
  const refBytes = fs.readFileSync(referencePath);
  assert.ok(refBytes.length > 0, 'reference wasm is empty: ' + referencePath);
  const Spid3r = require(path.join(root, 'web', 'spid3r.js'));
  const ref = Spid3r.initSync(refBytes);
  const refVer = ref.version();
  if (JSON.stringify(refVer) !== JSON.stringify(ver)) {
    console.error('FAIL: version mismatch — committed:', JSON.stringify(ver), 'fresh:', JSON.stringify(refVer));
    failures++;
  }
  for (const [name, source] of Object.entries(PRESETS)) {
    for (const engine of ['vm', 'tree']) {
      const r = ref.eval(source, engine);
      const committed = outputs[name][engine];
      const fresh = r.ok ? r.stdout : null;
      if (committed === null || fresh === null || committed !== fresh) {
        console.error(`FAIL ${name} [${engine}]: committed embedded module differs from the fresh build`);
        failures++;
      }
    }
  }
  console.log('ok committed embedded module matches the fresh build on every preset');
}

if (failures) process.exit(1);
console.log(`\nembedded wasm OK: ${Object.keys(PRESETS).length} presets on both engines` +
  (referencePath ? ', behaviorally equal to the fresh build' : ', both deployment copies current'));

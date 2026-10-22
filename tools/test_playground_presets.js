// Run every playground preset through the wasm runtime, so a stale or
// non-compiling snippet is caught here instead of by a user.
const fs = require('fs');
const path = require('path');
const Spid3r = require(path.join(__dirname, '..', 'web', 'spid3r.js'));

const root = path.join(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'docs', 'html', 'playground.html'), 'utf8');

// PRESETS is a JS object literal of template strings; evaluate it in a sandbox.
const start = html.indexOf('const PRESETS = {');
const endMarker = '\n      };';
const end = html.indexOf(endMarker, start);
if (start < 0 || end < 0) {
  console.error('could not locate PRESETS in playground.html');
  process.exit(1);
}
// slice through the closing brace, dropping the trailing semicolon
const literal = html
  .slice(start + 'const PRESETS = '.length, end + endMarker.length)
  .replace(/;\s*$/, '');
const PRESETS = eval('(' + literal + ')');

(async () => {
  const runtime = await Spid3r.init(fs.readFileSync(path.join(root, 'web', 'spid3r.wasm')));
  let failures = 0;
  for (const [name, source] of Object.entries(PRESETS)) {
    for (const engine of ['vm', 'tree']) {
      const res = runtime.eval(source, engine);
      if (!res.ok) {
        console.error(`FAIL ${name} [${engine}]: ${res.phase} ${res.error} (line ${res.line})`);
        failures++;
      }
    }
    const vm = runtime.eval(source, 'vm');
    const tree = runtime.eval(source, 'tree');
    if (vm.ok && tree.ok && vm.stdout !== tree.stdout) {
      console.error(`DIVERGENCE ${name}: vm and tree disagree`);
      failures++;
    }
    console.log(`ok ${name}`);
  }
  if (failures) process.exit(1);
  console.log(`\nall ${Object.keys(PRESETS).length} presets run on both engines`);
})();

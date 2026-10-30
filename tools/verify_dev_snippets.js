const fs = require('fs');
const path = require('path');
const Spid3r = require('../web/spid3r.js');

const wasmBuffer = fs.readFileSync(path.join(__dirname, '../web/spid3r.wasm'));
const runtime = Spid3r.initSync(wasmBuffer);
console.log('Runtime initialized successfully: v' + runtime.version().version);

const docPath = fs.existsSync(path.join(__dirname, '../docs/html/doc.html'))
  ? path.join(__dirname, '../docs/html/doc.html')
  : path.join(__dirname, '../docs/html/developer.html');
const html = fs.readFileSync(docPath, 'utf8');

// 1. Check all snippets in .w3-code-body
const regex = /<div class="w3-code-body"[^>]*>([\s\S]*?)<\/div>/g;
let match;
let count = 0;
let errors = [];

while ((match = regex.exec(html)) !== null) {
  count++;
  let raw = match[1]
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");

  // The snippet regex stops at the FIRST `</div>`, so an unclosed
  // `<div class="w3-code-body">` silently swallows whatever follows it (the
  // footer buttons, then the next card). That used to surface as
  // "unexpected character '⚡'" from the parser, which points at the emoji and
  // not at the missing tag. Report the structural cause instead.
  const strayTag = raw.match(/<\/?(div|button|span|a|p|ul|li)\b[^>]*>/i);
  if (strayTag) {
    errors.push({
      snippetIdx: count,
      codeSnippet: raw.substring(0, 100).replace(/\r?\n/g, ' '),
      structural: 'unclosed <div class="w3-code-body"> — captured markup `' +
        strayTag[0] + '`; add the missing </div> before the next sibling element',
      check: { ok: false },
      vm: { ok: false },
      tree: { ok: false }
    });
    continue;
  }

  const chk = runtime.check(raw);
  const vmEval = runtime.eval(raw, 'vm');
  const treeEval = runtime.eval(raw, 'tree');

  if (!chk.ok || !vmEval.ok || !treeEval.ok) {
    errors.push({
      snippetIdx: count,
      codeSnippet: raw.substring(0, 100).replace(/\r?\n/g, ' '),
      check: chk,
      vm: { ok: vmEval.ok, error: vmEval.error, phase: vmEval.phase, thrown: vmEval.thrown },
      tree: { ok: treeEval.ok, error: treeEval.error, phase: treeEval.phase, thrown: treeEval.thrown }
    });
  }
}

console.log('Snippet evaluation check: total ' + count + ', errors ' + errors.length);
// Print what actually failed: a bare count tells you the docs have drifted but
// not which snippet, which is the only thing you need in order to fix it.
for (const e of errors) {
  if (e.structural) {
    console.log('  [snippet ' + e.snippetIdx + '] MALFORMED HTML: ' + e.structural);
    console.log('    ' + e.codeSnippet);
    continue;
  }
  const which = !e.check.ok ? 'check' : (!e.vm.ok ? 'vm' : 'tree');
  const detail = which === 'check' ? (e.check.error || e.check.message)
    : which === 'vm' ? e.vm.error : e.tree.error;
  console.log('  [snippet ' + e.snippetIdx + '] ' + which + ' failed: ' + detail);
  console.log('    ' + e.codeSnippet);
}

// 2. Check all internal anchor links in developer.html
const idRegex = /id=["']([a-zA-Z0-9_\-]+)["']/g;
const ids = new Set();
let m;
while ((m = idRegex.exec(html)) !== null) {
  ids.add(m[1]);
}

const hrefRegex = /href=["']#([a-zA-Z0-9_\-]+)["']/g;
const brokenLinks = new Set();
while ((m = hrefRegex.exec(html)) !== null) {
  if (!ids.has(m[1])) {
    brokenLinks.add(m[1]);
  }
}

console.log('Total HTML IDs found:', ids.size);
console.log('Broken internal anchor links count:', brokenLinks.size);
if (brokenLinks.size > 0) {
  console.log('Broken target IDs:', Array.from(brokenLinks));
}

// 3. Test URL encoding round-trip for all 16 snippets
let roundTripFails = 0;
regex.lastIndex = 0;
while ((match = regex.exec(html)) !== null) {
  let raw = match[1]
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .trim();

  // playground encoding method:
  // encodeURIComponent(btoa(unescape(encodeURIComponent(raw))))
  const encoded = encodeURIComponent(Buffer.from(raw).toString('base64'));
  const decoded = Buffer.from(decodeURIComponent(encoded), 'base64').toString('utf8');
  if (decoded !== raw) roundTripFails++;
}
console.log('URL round-trip encoding failures:', roundTripFails);

// Exit non-zero when anything drifted, so this can gate CI. The docs are
// hand-maintained HTML with no generator, so this script is the only thing that
// notices when a snippet stops compiling or an anchor loses its target.
if (errors.length > 0 || brokenLinks.size > 0 || roundTripFails > 0) {
  process.exitCode = 1;
}

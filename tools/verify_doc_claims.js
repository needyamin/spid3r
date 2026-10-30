// Documentation claim gate.
//
// `tools/verify_dev_snippets.js` proves that the *code samples* in doc.html
// still compile and run. It does not prove that the *prose* around them is
// true — and every documentation defect found by the 2026-10-30 audit came
// from prose, not from a sample:
//
//   * chapter 11 claimed `bag` had set algebra (it does not)
//   * chapter 11 omitted 8 constructors that exist
//   * `DATA_STRUCTURES.md` claimed `m.sum(axis)` returns a 1xN matrix
//     (it returns a flat `vector`)
//   * chapter 11 did not say `list.remove` takes a value, not an index
//
// All four were invisible to the snippet check because no sample exercised
// them. This script closes that gap: it encodes the doc's factual claims as
// executable assertions, so the next time the runtime and the docs disagree,
// CI says which claim and which file.
//
// Two kinds of checks:
//   1. HTML structure  — tables must be well formed (no orphan rows, no
//      column-count drift, no duplicate ids). Structure is cheap to check and
//      is what silently broke when rows were deleted by hand.
//   2. Runtime claims  — each entry pairs a documented statement with a
//      program whose stdout must match. Run on the VM; the snippet gate
//      already pins VM/tree parity, so one engine is enough here.
//
// Adding a claim: copy an entry, cite the doc line in `doc`, and make the
// program print something narrow (a type name, a length, a single value)
// rather than a formatted dump — narrow output does not need updating every
// time a display format is polished.

const fs = require('fs');
const path = require('path');
const Spid3r = require('../web/spid3r.js');

const ROOT = path.join(__dirname, '..');
const DOC_HTML = path.join(ROOT, 'docs/html/doc.html');

const runtime = Spid3r.initSync(fs.readFileSync(path.join(ROOT, 'web/spid3r.wasm')));
const html = fs.readFileSync(DOC_HTML, 'utf8');

const failures = [];
function fail(area, message) {
  failures.push({ area, message });
}

// ---------------------------------------------------------------------------
// 1. HTML structure
// ---------------------------------------------------------------------------

// 1a. Orphan rows. A `<tr>` with no cells renders as a stray border and breaks
// anything that walks the DOM. These appear whenever a row is deleted by hand
// and the `<tr>`/`</tr>` pair is left behind.
const orphanRow = /<tr>[ \t]*\r?\n?[ \t]*<\/tr>/g;
let m;
let orphanCount = 0;
while ((m = orphanRow.exec(html)) !== null) {
  const line = html.slice(0, m.index).split('\n').length;
  orphanCount++;
  fail('doc.html:' + line, 'orphan <tr></tr> with no cells');
}
console.log('Empty <tr> rows:', orphanCount);

// 1b. Column-count drift. Every row in a table must have as many cells as the
// header row, otherwise the browser shifts the row and the table silently
// misrepresents the data. `colspan` is honoured so a legitimately merged cell
// does not trip the check.
const tableRe = /<table\b[\s\S]*?<\/table>/g;
let tables = 0;
let columnDrift = 0;
const cellCount = (rowHtml) => {
  const cells = rowHtml.match(/<(td|th)\b[^>]*>/g) || [];
  return cells.reduce((sum, tag) => {
    const span = /colspan\s*=\s*["']?(\d+)/i.exec(tag);
    return sum + (span ? parseInt(span[1], 10) : 1);
  }, 0);
};
const rowRe = /<tr\b[^>]*>[\s\S]*?<\/tr>/g;
let table;
while ((table = tableRe.exec(html)) !== null) {
  tables++;
  const rows = table[0].match(rowRe) || [];
  if (rows.length === 0) continue;
  const expected = cellCount(rows[0]);
  for (let i = 1; i < rows.length; i++) {
    const got = cellCount(rows[i]);
    if (got === 0) continue; // already reported as an orphan row
    if (got !== expected) {
      const offset = html.indexOf(table[0]) + table[0].indexOf(rows[i]);
      const line = html.slice(0, offset).split('\n').length;
      columnDrift++;
      fail('doc.html:' + line,
        'table row has ' + got + ' cell(s), header has ' + expected);
    }
  }
}
console.log('Tables checked:', tables, '| column-count drift:', columnDrift);

// 1c. Duplicate ids. Two elements sharing an id makes every `#anchor` link
// ambiguous and breaks the sidebar scroll spy.
const idRe = /\sid=["']([A-Za-z0-9_\-]+)["']/g;
const seen = new Map();
let duplicateIds = 0;
while ((m = idRe.exec(html)) !== null) {
  const id = m[1];
  if (seen.has(id)) {
    const line = html.slice(0, m.index).split('\n').length;
    duplicateIds++;
    fail('doc.html:' + line, 'duplicate id "' + id + '" (first at line ' + seen.get(id) + ')');
  } else {
    seen.set(id, html.slice(0, m.index).split('\n').length);
  }
}
console.log('Ids:', seen.size, '| duplicates:', duplicateIds);

// 1d. Image sources. Every `<img src>` and markdown image must resolve to a
// file that exists, relative to the page that references it. A renamed or
// moved diagram does not break the build — it just silently degrades to alt
// text — so this is checked explicitly. SVGs must also carry a <title>, which
// is what a screen reader announces in place of the image.
const imageChecks = [
  { file: DOC_HTML, base: path.dirname(DOC_HTML) },
  { file: path.join(ROOT, 'README.md'), base: ROOT },
];
let images = 0;
let missingImages = 0;
let untitledSvgs = 0;
const imgSrcRe = /<img\b[^>]*?\ssrc=["']([^"']+)["']/gi;
const mdImgRe = /!\[[^\]]*\]\(([^)\s]+)\)/g;
for (const { file, base } of imageChecks) {
  if (!fs.existsSync(file)) continue;
  const text = fs.readFileSync(file, 'utf8');
  const refs = [];
  while ((m = imgSrcRe.exec(text)) !== null) refs.push(m[1]);
  while ((m = mdImgRe.exec(text)) !== null) refs.push(m[1]);
  for (const ref of refs) {
    if (/^(https?:)?\/\//.test(ref) || ref.startsWith('data:')) continue;
    images++;
    const target = path.resolve(base, decodeURI(ref));
    if (!fs.existsSync(target)) {
      missingImages++;
      fail(path.basename(file), 'image does not exist: ' + ref);
      continue;
    }
    if (target.endsWith('.svg')) {
      const svg = fs.readFileSync(target, 'utf8');
      if (!/<title>[\s\S]*?<\/title>/.test(svg)) {
        untitledSvgs++;
        fail(path.basename(target), 'SVG has no <title>, so screen readers announce nothing');
      }
    }
  }
}
console.log('Images checked:', images, '| missing:', missingImages, '| SVGs without title:', untitledSvgs);

// ---------------------------------------------------------------------------
// 2. Runtime claims
// ---------------------------------------------------------------------------

const CONTAINERS = [
  // chapter 11 "Constructor" column + docs/DATA_STRUCTURES.md tables
  ['list(1, 2)', 'list'],
  ['map("k", 1)', 'map'],
  ['set(1, 2)', 'set'],
  ['bag()', 'bag'],
  ['tuple(1, "a")', 'tuple'],
  ['bytes("s")', 'bytes'],
  ['stack()', 'stack'],
  ['queue()', 'queue'],
  ['deque()', 'deque'],
  ['heap("min")', 'heap'],
  ['priority_queue()', 'priority_queue'],
  ['ring_buffer(4)', 'ring_buffer'],
  ['linked_list()', 'linked_list'],
  ['hash_table(8)', 'hash_table'],
  ['bst()', 'bst'],
  ['binary_tree()', 'binary_tree'],
  ['trie()', 'trie'],
  ['graph("directed")', 'graph'],
  ['matrix(2, 2, 0.0)', 'matrix'],
  ['vector(1, 2)', 'vector'],
];

const CLAIMS = [
  // docs/DATA_STRUCTURES.md:262-263 — axis reductions collapse to a flat
  // vector, NOT a 1xN / Mx1 matrix. This claim is what the audit caught.
  {
    doc: 'docs/DATA_STRUCTURES.md:262',
    claim: 'm.sum(0) on a 2x3 matrix returns a vector of length cols (3)',
    src: 'fn main() { let m = matrix(2, 3, 1.0); let s = m.sum(0); println(type_of(s)); println(len(s)); return 0; }',
    expect: 'vector\n3',
  },
  {
    doc: 'docs/DATA_STRUCTURES.md:262',
    claim: 'm.sum(1) on a 2x3 matrix returns a vector of length rows (2)',
    src: 'fn main() { let m = matrix(2, 3, 1.0); let s = m.sum(1); println(type_of(s)); println(len(s)); return 0; }',
    expect: 'vector\n2',
  },
  {
    doc: 'docs/DATA_STRUCTURES.md:262',
    claim: 'm.sum(null) returns a plain float',
    src: 'fn main() { println(type_of(matrix(2, 3, 1.0).sum(null))); return 0; }',
    expect: 'float',
  },
  {
    doc: 'docs/DATA_STRUCTURES.md:263',
    claim: 'm.mean(0) returns a vector of length cols (3)',
    src: 'fn main() { let s = matrix(2, 3, 1.0).mean(0); println(type_of(s)); println(len(s)); return 0; }',
    expect: 'vector\n3',
  },
  {
    doc: 'docs/DATA_STRUCTURES.md:263',
    claim: 'm.mean(1) returns a vector of length rows (2)',
    src: 'fn main() { let s = matrix(2, 3, 1.0).mean(1); println(type_of(s)); println(len(s)); return 0; }',
    expect: 'vector\n2',
  },
  {
    doc: 'docs/DATA_STRUCTURES.md:263',
    claim: 'm.mean(null) returns a plain float',
    src: 'fn main() { println(type_of(matrix(2, 3, 1.0).mean(null))); return 0; }',
    expect: 'float',
  },

  // doc.html:3217 — list.remove takes a VALUE, list.remove_at takes an INDEX.
  {
    doc: 'doc.html:3217',
    claim: 'list.remove(value) removes by value and returns the removed value',
    src: 'fn main() { let l = [10, 20, 30]; println(l.remove(20)); println(string(l)); return 0; }',
    expect: '20\n[10, 30]',
  },
  {
    doc: 'doc.html:3217',
    claim: 'list.remove_at(index) removes by index and returns the removed value',
    src: 'fn main() { let l = [10, 20, 30]; println(l.remove_at(0)); println(string(l)); return 0; }',
    expect: '10\n[20, 30]',
  },

  // docs/STANDARD_LIBRARY.md:817 — container display formats.
  {
    doc: 'docs/STANDARD_LIBRARY.md:817',
    claim: 'set prints as set{...}',
    src: 'fn main() { println(string(set(1, 2))); return 0; }',
    expect: 'set{1, 2}',
  },
  {
    doc: 'docs/STANDARD_LIBRARY.md:817',
    claim: 'map prints as map{k: v}',
    src: 'fn main() { println(string(map("a", 1))); return 0; }',
    expect: 'map{a: 1}',
  },
  {
    doc: 'docs/STANDARD_LIBRARY.md:817',
    claim: 'list prints as [a, b]',
    src: 'fn main() { println(string([1, 2])); return 0; }',
    expect: '[1, 2]',
  },

  // doc.html:3250 — "No set algebra — use set for that." Negative claim: the
  // bag must NOT expose union, or the doc is wrong in the other direction.
  {
    doc: 'doc.html:3250',
    claim: 'bag has no set algebra (union must be an unknown method)',
    src: 'fn main() { let b = bag(1); let u = b.union(bag(2)); return 0; }',
    expectError: /unknown method 'union'/,
  },
  // doc.html:3247 — `bag()` takes no constructor arguments; members go in via
  // `add`, and `count_of` reports duplicates (multiset semantics).
  {
    doc: 'doc.html:3247',
    claim: 'bag.count_of counts duplicates (multiset semantics)',
    src: 'fn main() { let b = bag(); b.add(1); b.add(1); b.add(2); println(b.count_of(1)); return 0; }',
    expect: '2',
  },

  // doc.html:1697 — the GC is reachable from a documented path; the value it
  // reports for a live object must survive a collection.
  {
    doc: 'doc.html:1697',
    claim: 'gc_collect keeps reachable values intact',
    src: 'fn main() { let l = [1, 2, 3]; gc_collect(); println(string(l)); return 0; }',
    expect: '[1, 2, 3]',
  },

  // Chapter 14 export-name table — the module prefix is part of the name, so
  // the bare spelling the audit found in the prose must stay rejected.
  {
    doc: 'doc.html (stdlib export names)',
    claim: 'toml.toml_parse parses and indexes by key',
    src: 'import "toml" as toml;\nfn main() { let m = toml.toml_parse("a = 1"); println(m["a"]); return 0; }',
    expect: '1',
  },
  {
    doc: 'doc.html (stdlib export names)',
    claim: 'uuid4() is NOT a global — the export is uuid.uuid_v4()',
    src: 'fn main() { let u = uuid4(); return 0; }',
    expectError: /unknown name 'uuid4'/,
  },
  {
    doc: 'doc.html (stdlib export names)',
    claim: 'bcrypt_hash rejects cost outside 4..31',
    src: 'fn main() { let h = bcrypt_hash("pw", 3); return 0; }',
    expectError: /cost must be 4\.\.31/,
  },
];

// Constructors: each documented constructor must be callable and must produce
// the type the docs name for it.
for (const [expr, typeName] of CONTAINERS) {
  CLAIMS.push({
    doc: 'doc.html:3214+ / docs/DATA_STRUCTURES.md',
    claim: expr + ' constructs a ' + typeName,
    src: 'fn main() { let v = ' + expr + '; println(type_of(v)); return 0; }',
    expect: typeName,
  });
}

let claimFails = 0;
for (const c of CLAIMS) {
  const r = runtime.eval(c.src, 'vm');
  let ok;
  let detail = '';
  if (c.expectError) {
    ok = !r.ok && c.expectError.test((r.error || '') + ' ' + (r.thrown || ''));
    if (!ok) {
      detail = r.ok
        ? 'expected a "' + c.expectError + '" error, but the program succeeded'
        : 'wrong error: ' + (r.error || r.thrown);
    }
  } else {
    ok = r.ok && (r.stdout || '').replace(/\r/g, '').trim() === c.expect;
    if (!ok) {
      detail = r.ok
        ? 'stdout ' + JSON.stringify((r.stdout || '').trim()) + ' != ' + JSON.stringify(c.expect)
        : 'did not run: ' + (r.error || r.thrown);
    }
  }
  if (!ok) {
    claimFails++;
    fail(c.doc, c.claim + ' — ' + detail);
  }
}
console.log('Runtime claims:', CLAIMS.length, '| failures:', claimFails);

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

if (failures.length > 0) {
  console.log('\nDocumentation claim failures (' + failures.length + '):');
  for (const f of failures) {
    console.log('  ' + f.area + ': ' + f.message);
  }
  console.log('\nFix the doc, or — if the runtime is the one that changed on ' +
    'purpose — update the claim and its citation together.');
  process.exitCode = 1;
} else {
  console.log('\nAll documentation claims hold.');
}

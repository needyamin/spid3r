const fs = require('fs');
const wasmBytes = fs.readFileSync('web/spid3r.wasm');
const b64 = wasmBytes.toString('base64');
const content = `// Auto-generated embedded base64 WebAssembly binary for Spid3r
// Enables docs/html/playground.html to run 100% offline and standalone even on file:// URLs
(function(g) {
  g.__SPID3R_WASM_BASE64__ = "${b64}";
})(typeof window !== 'undefined' ? window : (typeof globalThis !== 'undefined' ? globalThis : this));
`;
fs.writeFileSync('web/spid3r_wasm_data.js', content, 'utf8');
console.log('Successfully generated web/spid3r_wasm_data.js (' + (content.length / 1024 / 1024).toFixed(2) + ' MB)');

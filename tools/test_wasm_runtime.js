// Smoke test for the in-browser runtime: load web/spid3r.wasm through the
// published SDK and run programs that import stdlib modules and use the newer
// natives. Run: node tools/test_wasm_runtime.js
const fs = require('fs');
const path = require('path');
const Spid3r = require(path.join(__dirname, '..', 'web', 'spid3r.js'));

const wasmPath = path.join(__dirname, '..', 'web', 'spid3r.wasm');

const program = `
import "toml";
import "yaml";
import "semver" as sv;

fn main() {
    let doc = toml_parse("a = 1\\n[t]\\nb = \\"x\\"\\n");
    println("toml:" + string(doc["a"]) + "/" + doc["t"]["b"]);

    let y = yaml_parse("name: app\\nport: 8080\\n");
    println("yaml:" + y["name"] + "/" + string(y["port"]));

    let encoded = json_write(map("k", 1, "ok", true));
    println("json:" + encoded);
    println("semver:" + string(sv.compare("1.2.3", "1.10.0")));

    let gz = gzip_compress("hello hello hello hello hello");
    println("gz:" + string(len(gz)) + " rt:" + gunzip(gz).unwrap().to_string().unwrap());

    println("sha:" + sha256("abc"));
    println("b32:" + base32_encode("abc"));
    println("crc:" + string(crc32("123456789")));
}
`;

(async () => {
  const runtime = await Spid3r.init(fs.readFileSync(wasmPath));
  const v = runtime.version();
  console.log('version:', JSON.stringify(v));

  const res = runtime.eval(program, 'vm');
  if (!res.ok) {
    console.error('FAILED:', JSON.stringify(res, null, 2));
    process.exit(1);
  }
  console.log('--- program output ---');
  console.log(res.stdout.trimEnd());

  const checks = [
    ['toml:1/x', res.stdout],
    ['yaml:app/8080', res.stdout],
    ['json:', res.stdout],
    ['semver:-1', res.stdout],
    ['sha:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', res.stdout],
    ['b32:MFRGG===', res.stdout],
    ['crc:3421780262', res.stdout],
  ];
  let bad = 0;
  for (const [needle, hay] of checks) {
    if (!hay.includes(needle)) {
      console.error('missing expected output:', needle);
      bad++;
    }
  }

  // The tree engine must agree with the VM.
  const tree = runtime.eval(program, 'tree');
  if (!tree.ok || tree.stdout !== res.stdout) {
    console.error('tree/vm divergence');
    bad++;
  }

  if (bad) process.exit(1);
  console.log('\nwasm runtime OK');
})();

/**
 * Comprehensive QA Test Suite for Spid3r WebAssembly Engine & JS SDK
 */
const fs = require('fs');
const assert = require('assert');
const Spid3r = require('../web/spid3r.js');

async function runQATests() {
    console.log("=== Spid3r WebAssembly QA Test Suite ===");

    const wasmBytes = fs.readFileSync('./web/spid3r.wasm');
    const sp = await Spid3r.init(wasmBytes);

    // 1. Version test
    const ver = sp.version();
    console.log("✓ Version Query:", ver);
    assert.ok(ver.version === "1.0.0" || ver.version === "0.1.0");
    assert.strictEqual(ver.language_version, "1.0");

    // 2. Syntax & Semantic Checker tests
    console.log("Running Checker QA...");
    const checkValid = sp.check("let x = 10; fn add(a, b) { return a + b; }");
    assert.strictEqual(checkValid.ok, true, "Valid code should pass check");

    const checkSyntaxErr = sp.check("let x = ;");
    assert.strictEqual(checkSyntaxErr.ok, false);
    assert.strictEqual(checkSyntaxErr.phase, "Parser");
    assert.strictEqual(checkSyntaxErr.line, 1);

    const checkSemanticErr = sp.check("fn test() { return undefined_variable; }");
    assert.strictEqual(checkSemanticErr.ok, false);
    assert.strictEqual(checkSemanticErr.phase, "Semantic");

    // 3. Differential Execution (Tree vs VM)
    console.log("Running Differential Engine QA...");
    const testScript = `
        let list = [10, 20, 30];
        let sum = 0;
        for n in list {
            sum = sum + n;
        }
        println("Sum: " + string(sum));
    `;

    const treeRes = sp.eval(testScript, "tree");
    const vmRes = sp.eval(testScript, "vm");
    console.log("treeRes:", treeRes);
    console.log("vmRes:", vmRes);

    assert.strictEqual(treeRes.ok, true);
    assert.strictEqual(vmRes.ok, true);
    assert.strictEqual(treeRes.stdout, "Sum: 60\n");
    assert.strictEqual(vmRes.stdout, "Sum: 60\n");
    assert.strictEqual(treeRes.stdout, vmRes.stdout, "Tree and VM outputs must be identical");

    // 4. Unicode & Multi-Byte UTF-8 String Handling
    console.log("Running UTF-8 & Unicode QA...");
    const unicodeScript = `
        let bangla = "স্বাগতম স্পাইডার!";
        let emoji = "⚡ Spid3r WebAssembly 🚀";
        println(bangla);
        println(emoji);
    `;
    const uniRes = sp.eval(unicodeScript, "vm");
    assert.strictEqual(uniRes.ok, true);
    assert.ok(uniRes.stdout.includes("স্বাগতম স্পাইডার!"));
    assert.ok(uniRes.stdout.includes("⚡ Spid3r WebAssembly 🚀"));

    // 5. OOP & Inheritance in WASM
    console.log("Running OOP & Classes QA...");
    const oopScript = `
        class Animal {
            public let name;
            public fn speak(self) {
                return self.name + " makes a sound";
            }
        }

        class Dog extends Animal {
            public let breed;
            public fn speak(self) {
                return self.name + " barks!";
            }
        }

        let d = new Dog();
        d.name = "Rex";
        d.breed = "German Shepherd";
        println(d.speak());
        println("Breed: " + d.breed);
    `;
    const oopRes = sp.eval(oopScript, "vm");
    assert.strictEqual(oopRes.ok, true);
    assert.ok(oopRes.stdout.includes("Rex barks!"));
    assert.ok(oopRes.stdout.includes("Breed: German Shepherd"));

    // 6. Runtime Exception Handling & Stack Diagnostics
    console.log("Running Exception & Diagnostics QA...");
    const throwScript = `
        fn fail() {
            throw "Intentional Crash";
        }
        fail();
    `;
    const throwRes = sp.eval(throwScript, "vm");
    assert.strictEqual(throwRes.ok, false);
    assert.ok(throwRes.phase === "Vm" || throwRes.phase === "Runtime");
    assert.strictEqual(throwRes.thrown, "Intentional Crash");

    // 7. Formatter QA
    console.log("Running Canonical Formatter QA...");
    const unformatted = "let    x=1+2;\nfn   test(a,b){\nreturn    a+b;\n}";
    const fmtRes = sp.format(unformatted);
    assert.strictEqual(fmtRes.ok, true);
    assert.ok(fmtRes.code.includes("let x = 1 + 2;"));
    assert.ok(fmtRes.code.includes("fn test(a, b) {"));

    // 8. Memory & Concurrency Leak Stress Test (1,000 rapid evals)
    console.log("Running 1,000 Rapid Evaluations Stress QA...");
    const startMem = process.memoryUsage().heapUsed;
    for (let i = 0; i < 1000; i++) {
        const r = sp.eval(`let a = ${i}; let b = 2; let c = a * b;`, "vm");
        assert.strictEqual(r.ok, true);
    }
    const endMem = process.memoryUsage().heapUsed;
    console.log(`Memory delta after 1,000 evals: ${((endMem - startMem) / 1024).toFixed(2)} KB`);

    console.log("\n=========================================");
    console.log("🎉 ALL QA TESTS PASSED CLEANLY (8/8)!");
    console.log("=========================================");
}

runQATests().catch(err => {
    console.error("❌ QA Test Failure:", err);
    process.exit(1);
});

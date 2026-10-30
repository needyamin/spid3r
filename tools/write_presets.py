#!/usr/bin/env python3
"""Splice a comprehensive PRESETS block into docs/html/playground.html.

The presets are plain Spid3r sources here; the script emits them as JS
template literals (the playground's format) with the escaping the page needs:
every backslash doubles, so a matrix string's newline escape reaches the
lexer as the two characters it expects:
  \\  -> \\\\      (a matrix "\\n" stays a backslash-n escape for the lexer)
  `  -> \\`
  ${ -> \\${   (never interpolate)
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BACKSLASH = chr(92)  # kept as a constant so bulk edits cannot corrupt the literals

# ---------------------------------------------------------------- presets
# Every snippet below was executed against the embedded wasm runtime on both
# engines (see tools/probe_presets*.js) and prints deterministic output only.

HELLO = r'''// 1. Hello World & System Info
println("=========================================");
println("  ⚡ Welcome to Spid3r WebAssembly Engine!");
println("=========================================");

let name = "Browser Developer";
let version = 1.0;

println("Hello, " + name + "!");
println("Language Version: ", version);

// Test math & collections
let numbers = [10, 20, 30, 40, 50];
let total = 0;
for n in numbers {
    total = total + n;
}
println("Sum of array:", total);
'''

FIB = r'''// 2. Recursive Fibonacci
fn fib(n) {
    if (n <= 1) {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

println("Computing Fibonacci sequence:");
for let i = 0; i <= 12; i = i + 1 {
    println("fib(" + string(i) + ") = " + string(fib(i)));
}
'''

CONTROL = r'''// 3. Control Flow: if / elif / while / for
fn sign(n) {
    if (n < 0) {
        return "negative";
    } elif (n == 0) {
        return "zero";
    }
    return "positive";
}

// if is also an expression
fn grade(n) {
    return if (n >= 90) { "A" } else if (n >= 80) { "B" } else { "C" };
}

println("sign(-3) = " + sign(-3));
println("sign(0)  = " + sign(0));
println("sign(7)  = " + sign(7));
println("grade(95) = " + grade(95) + ", grade(85) = " + grade(85));

// while with break and continue: the Collatz sequence
let n = 100;
let steps = 0;
while (n != 1) {
    if (n % 2 == 0) {
        n = n / 2;
    } else {
        n = 3 * n + 1;
    }
    steps = steps + 1;
}
println("collatz(100) takes " + string(steps) + " steps");

// for over a list, skipping even values
let odd_sum = 0;
for x in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
    if (x % 2 == 0) {
        continue;
    }
    odd_sum = odd_sum + x;
}
println("sum of odd 1..10 = " + string(odd_sum));
'''

FUNCTIONS = r'''// 4. Functions & Closures
fn make_counter(start) {
    let count = start;
    return fn() {
        count = count + 1;
        return count;
    };
}

let c = make_counter(100);
println("counter: " + string(c()) + ", " + string(c()) + ", " + string(c()));

// Functions are values: pass one to another
fn apply_twice(f, x) {
    return f(f(x));
}
println("apply_twice(double, 21) = " + string(apply_twice(fn(x) { return x * 2; }, 21)));

// Closures nest and capture every level
fn adder(x) {
    return fn(y) {
        return fn(z) {
            return x + y + z;
        };
    };
}
println("adder(1)(2)(3) = " + string(adder(1)(2)(3)));

// Mutual recursion
fn is_even(n) {
    if (n == 0) {
        return true;
    }
    return is_odd(n - 1);
}

fn is_odd(n) {
    if (n == 0) {
        return false;
    }
    return is_even(n - 1);
}
println("is_even(10) = " + string(is_even(10)) + ", is_odd(7) = " + string(is_odd(7)));

// Deep recursion stays on the big stack
fn fact(n) {
    if (n <= 1) {
        return 1;
    }
    return n * fact(n - 1);
}
println("fact(20) = " + string(fact(20)));
'''

STRINGS = r'''// 5. Strings & Interpolation
let s = "  Hello, World  ";
println("trim+lower: [" + s.trim().lower() + "]");
println("replace:    " + "a-b-c".replace("-", "+"));
println("slice(1,3): " + "hello".slice(1, 3));
println("split:      " + string("a,b,c".split(",")));
println("starts_with/ends_with/contains: " + string("hello".starts_with("he")) + "/" + string("hello".ends_with("lo")) + "/" + string("hello".contains("ell")));
println("index [1]:  " + "hello"[1]);

// Interpolation evaluates any expression
let items = [1, 2, 3];
println("list has {len(items)} items: {items}");

// format fills {} placeholders in order
println("{} and {}".format("a", "b"));

// Comparison is lexicographic
println("a < b: " + string("a" < "b"));
'''

COLLECTIONS = r'''// 6. Lists, Maps & Sets
let xs = [5, 3, 8, 1];
println("original: " + string(xs));
xs.sort();              // in place, returns null
println("sorted:   " + string(xs));
xs.reverse();
println("reversed: " + string(xs));
println("slice(1,3): " + string(xs.slice(1, 3)) + ", index_of(8): " + string(xs.index_of(8)));

// Higher-order methods chain
let odds = [1, 2, 3, 4, 5].filter(fn(x) { return x % 2 == 1; });
println("filter odds:    " + string(odds));
println("map squares:    " + string(odds.map(fn(x) { return x * x; })));

// Maps keep insertion order
let m = map("a", 1, "b", 2);
m["c"] = 3;
println("keys: " + string(m.keys()) + ", has(a): " + string(m.has("a")) + ", len: " + string(len(m)));

// Set algebra
let s1 = set(1, 2, 3);
let s2 = set(2, 3, 4);
println("union:      " + string(s1.union(s2)));
println("intersect:  " + string(s1.intersect(s2)));

// Tuples index like lists and destructure
let t = tuple(1, "two", 3.0);
println("tuple: " + string(t[0]) + "/" + t[1] + "/" + string(t[2]));
let (x, y) = tuple(10, 20);
println("destructured: " + string(x + y));
'''

DATASTRUCTURES = r'''// 7. Built-in Data Structures
let s = stack();
s.push(1);
s.push(2);
s.push(3);
println("stack pop: " + string(s.pop()));

let q = queue();
q.push(1);
q.push(2);
println("queue pop: " + string(q.pop()) + ", len: " + string(len(q)));

let d = deque();
d.push_back(2);
d.push_front(1);
println("deque len: " + string(len(d)));

let h = heap("max");
h.push(3);
h.push(9);
h.push(1);
println("max-heap pop: " + string(h.pop()));

let pq = priority_queue();
pq.push("low", 5);
pq.push("high", 1);
println("priority pop: " + pq.pop());

let rb = ring_buffer(3);
rb.push(1);
rb.push(2);
println("ring buffer: len " + string(len(rb)) + ", pop " + string(rb.pop()));

let ll = linked_list();
ll.push_back(1);
ll.push_back(2);
println("linked list len: " + string(len(ll)));

let ht = hash_table();
ht.set("a", 1);
println("hash table get: " + string(ht.get("a")));

let bt = binary_tree();
bt.insert(5);
bt.insert(3);
bt.insert(8);
println("binary tree len: " + string(len(bt)));

let tr = trie("");
tr.insert("ab");
println("trie len: " + string(len(tr)));

let g = graph("directed");
g.add_node("a");
g.add_node("b");
g.add_edge("a", "b", 1);
println("graph nodes: " + string(len(g)));
'''

OOP = r'''// 8. Classes & Inheritance
class Shape {
    public let name;
    fn new(name) {
        self.name = name;
    }
    public fn describe(self) {
        return "Shape: " + self.name;
    }
}

class Circle extends Shape {
    public let radius;
    fn new(name, radius) {
        super(name);
        self.radius = radius;
    }
    public fn area(self) {
        return 3.14159 * self.radius * self.radius;
    }
}

let c = new Circle("MyCircle", 5.0);
println(c.describe());
println("radius: " + string(c.radius));
println("area:   " + string(c.area()));

// Abstract classes declare, subclasses implement
abstract class Polygon {
    abstract fn area(self);
    public fn kind(self) {
        return "polygon";
    }
}

class Square extends Polygon {
    public let side;
    public fn area(self) {
        return self.side * self.side;
    }
}

let sq = new Square();
sq.side = 3.0;
println("square area: " + string(sq.area()) + ", kind: " + sq.kind());

// Interfaces are contracts
interface Greet {
    fn hello(self) -> string;
}

class En implements Greet {
    public fn hello(self) {
        return "hi from En";
    }
}
println(new En().hello());
'''

OPERATORS = r'''// 9. Operator Overloading
class Vec2 {
    public let x;
    public let y;
    public fn __add__(self, o) {
        let v = new Vec2();
        v.x = self.x + o.x;
        v.y = self.y + o.y;
        return v;
    }
    public fn dist(self) {
        return sqrt(self.x * self.x + self.y * self.y);
    }
}

let a = new Vec2();
a.x = 3;
a.y = 4;
let b = new Vec2();
b.x = 1;
b.y = 1;

let sum = a + b;
println("a = (" + string(a.x) + ", " + string(a.y) + ")");
println("b = (" + string(b.x) + ", " + string(b.y) + ")");
println("a + b = (" + string(sum.x) + ", " + string(sum.y) + ")");
println("|a| = " + string(a.dist()));
'''

ENUMS = r'''// 10. Enums & Pattern Matching
enum Shape {
    Empty,
    Circle(radius),
    Rect(w, h),
}

fn area(s) {
    return match (s) {
        Circle(r) => 3 * r * r,
        Rect(w, h) => w * h,
        Empty() => 0,
    };
}

println("circle(2):  " + string(area(Circle(2.0))));
println("rect(2,3):  " + string(area(Rect(2.0, 3.0))));
println("empty:      " + string(area(Empty())));

// Literals and the wildcard arm
fn classify(n) {
    return match (n) {
        0 => "zero",
        1 => "one",
        _ => "many",
    };
}
println(classify(0) + "/" + classify(1) + "/" + classify(42));

// Guards narrow an arm; a failed guard falls through
fn size(s) {
    return match (s) {
        Circle(r) if (r > 10) => "big",
        Circle(r) => "small",
        Rect(w, h) => "rect",
        Empty() => "empty",
    };
}
println(size(Circle(2.0)) + "/" + size(Circle(20.0)) + "/" + size(Rect(1.0, 1.0)));

// Patterns nest
enum At {
    At(int, int),
}

enum Dot {
    Dot(At),
}

fn manhattan(d) {
    return match (d) {
        Dot(At(x, y)) => x + y,
    };
}
println("manhattan: " + string(manhattan(Dot(At(2, 3)))));
'''

ERRORS = r'''// 11. Error Handling
fn parse(s) {
    if (s == "42") {
        return ok(42);
    }
    return err("not a number");
}

// Option and result are method-based
let a = some(5);
println("some: is_some " + string(a.is_some()) + ", unwrap " + string(a.unwrap()) + ", map " + string(a.map(fn(x) { return x * 2; }).unwrap()));
let b = none();
println("none: is_none " + string(b.is_none()) + ", unwrap_or " + string(b.unwrap_or(99)));

let r = parse("42");
println("ok:   is_ok " + string(r.is_ok()) + ", unwrap " + string(r.unwrap()));
let e = parse("nope");
println("err:  is_err " + string(e.is_err()) + ", message " + e.unwrap_err());

// Combinators chain
println("and_then: " + string(ok("42").and_then(parse).unwrap()));

// try / catch / finally
fn boom() {
    throw "kaboom";
}

try {
    boom();
} catch (e) {
    println("caught: " + e);
} finally {
    println("finally always runs");
}

// is_failure checks both wrappers
println("is_failure: " + string(is_failure(err("x"))) + "/" + string(is_failure(ok(1))) + "/" + string(is_failure(none())));
'''

CONCURRENCY = r'''// 12. Concurrency: tasks, channels, shared state
fn double(n) {
    return n * 2;
}

// Spawn anonymous tasks and join them
let t1 = spawn fn() { return double(21); };
let t2 = spawn fn() { return 7; };
println("t1: " + string(t1.join()) + ", t2: " + string(t2.join()));
println("status after join: " + t1.status());

// A failed task re-raises on join, so it stays catchable
let bad = spawn fn() { throw "task-error"; };
try {
    bad.join();
} catch (e) {
    println("caught: " + e);
}
println("bad status: " + bad.status());

// Channels are unbounded FIFO queues
let ch = channel();
spawn fn() {
    ch.send(1);
    ch.send(2);
    ch.send(3);
};
println("recv: " + string(ch.recv()) + string(ch.recv()) + string(ch.recv()));

// Shared state behind a mutex
let shared = mutex(list());
let items = shared.lock();
items.push(1);
items.push(2);
shared.unlock();
let again = shared.lock();
println("shared len: " + string(len(again)));
shared.unlock();

// await a task from async code
async fn settle(t) {
    return await t;
}
let t3 = spawn fn() { return 99; };
println("settled: " + string(settle(t3)));
'''

TYPING = r'''// 13. Gradual Typing
fn add(a: int, b: int) -> int {
    return a + b;
}

let xs: list = [1, 2];
println("add(2, 3) = " + string(add(2, 3)) + ", len(xs) = " + string(len(xs)));

// Explicit casts
println(string(3 as float) + "/" + string(2.7 as int) + "/" + string(true as string));

// type_of reports the runtime type
println(type_of(1) + "/" + type_of("s") + "/" + type_of(1.5) + "/" + type_of(true) + "/" + type_of(null));
println(type_of([1]) + "/" + type_of(map("a", 1)) + "/" + type_of(set(1)) + "/" + type_of(tuple(1)));

// const bindings
const PI = 3.14159;
println("PI = " + string(PI));
'''

NUMERIC = r'''// 14. Math & Numeric Types
import "bigint" as bi;
import "decimal" as dec;
import "rational" as ra;
import "complex" as cx;
import "stats" as st;

println(string(sqrt(16)) + "/" + string(pow(2, 10)) + "/" + string(abs(-5)));
println(string(floor(2.7)) + "/" + string(ceil(2.1)) + "/" + string(round(2.5)));
println(string(sin(0)) + "/" + string(cos(0)));

// Bitwise operators and functions
println(string(12 & 10) + "/" + string(12 | 10) + "/" + string(12 ^ 10) + "/" + string(1 << 4) + "/" + string(256 >> 4));
println(string(bit_and(12, 10)) + "/" + string(shl(1, 4)));

// Arbitrary precision and exact numeric types
let big = bi.add(bi.from_string("123456789012345678901234567890"), bi.from_int(1));
println("bigint:  " + bi.to_string(big));

println("decimal: " + dec.to_string(dec.mul(dec.from_string("1.10"), dec.from_string("3"))));

println("rational: " + ra.to_string(ra.div(ra.of(1, 3), ra.of(1, 6))));

println("complex: " + cx.to_string(cx.add(cx.of(1, 2), cx.of(3, 4))));

// Statistics and signal processing
let data = [1, 2, 3, 4];
println("mean: " + string(st.mean(data)) + ", median: " + string(st.median([3, 1, 2])));
println("variance: " + string(st.variance(data, 1)) + ", p50: " + string(st.percentile(data, 50)));

let spec = fft([1.0, 2.0, 3.0, 4.0]);
println("fft: n=" + string(spec["n"]) + ", bins=" + string(len(spec["re"])));
'''

FORMATS = r'''// 15. Serialization Formats
import "toml";
import "yaml" as yml;
import "xml";

// JSON
let v = json_parse("{\"n\":1,\"a\":[1,2]}");
println("json: " + json_write(v));

// TOML parses and writes
let cfg = toml_parse("name = \"spid3r\"\n[server]\nport = 8080\n");
println("toml: " + string(cfg["name"]) + " / " + string(toml_get(cfg, "server.port")));
println(toml_write(cfg));

// YAML
let doc = yml.yaml_parse("host: a.io\nports:\n  - 80\n  - 443\n");
println("yaml: " + string(doc["host"]) + " / " + string(doc["ports"][1]));

// CSV
let rows = parse_csv("a,b\n1,2\n3,4\n").unwrap();
println("csv: " + string(len(rows["rows"])) + " rows, first cell " + string(rows["rows"][0][1]));

// XML
let d = xml_parse("<a x=\"1\"><b>hi</b></a>");
println("xml: " + d["name"] + " / " + d["children"][0]["text"]);

// Binary formats: msgpack, cbor, protobuf
let mp = msgpack_decode(msgpack_encode(map("k", 1))).unwrap();
println("msgpack:   " + string(mp["k"]));
let cb = cbor_decode(cbor_encode(map("k", 1))).unwrap();
println("cbor:      " + string(cb["k"]));
let pb = pb_decode(pb_encode(map(1, 42))).unwrap();
println("protobuf:  " + string(pb[1][0]));

// ZIP archives
let z = zip_create([map("name", "f.txt", "data", bytes([104, 105]))]);
let files = zip_read(z).unwrap();
println("zip: " + files["f.txt"].to_string().unwrap());
'''

CRYPTO = r'''// 16. Crypto & Encoding
println("sha256(abc): " + sha256("abc"));
println("hmac_sha256: " + hmac_sha256("key", "msg"));
println("adler32(hello): " + string(adler32("hello")));
println("secure_compare: " + string(secure_compare("abc", "abc")) + "/" + string(secure_compare("abc", "abd")));

// Encodings round-trip
println("base64: " + base64_encode("hello"));
println("base32: " + base32_encode("hello world"));
let b64 = base64_decode(base64_encode("hello"));
println("base64 round trip: " + b64.to_string().unwrap());
let b32 = base32_decode(base32_encode("hello world"));
println("base32 round trip: " + b32.to_string().unwrap());

let raw = bytes([1, 2, 255]);
println("hex: " + hex_encode(raw) + " -> " + string(len(hex_decode(hex_encode(raw)))) + " bytes");
println("url: " + url_encode("a b&c=d") + " -> " + url_decode(url_encode("a b&c=d")));
'''

COMPRESSION = r'''// 17. Compression
let text = "the quick brown fox jumps over the lazy dog. ";
let big = text + text + text + text;

let gz = gzip_compress(big);
println("gzip: " + string(len(big)) + " -> " + string(len(gz)) + " bytes");
println("gunzip round trip:  " + string(gunzip(gz).unwrap().to_string().unwrap() == big));

let zl = zlib_compress(big);
println("zlib round trip:    " + string(unzlib(zl).unwrap().to_string().unwrap() == big));

let df = deflate(big);
println("deflate round trip: " + string(inflate(df).unwrap().to_string().unwrap() == big));
'''

TEXT = r'''// 18. Text Processing
// Regex natives take (pattern, text)
println("match:    " + string(regex_match("[0-9]+", "a1b2")));
println("find:     " + string(regex_find("[0-9]+", "a1b2")));
println("find_all: " + string(len(regex_find_all("[0-9]", "a1b2"))));
println("replace:  " + regex_replace("[0-9]", "a1b2", "#"));

// Unicode case folding and normalization
println("casefold:   " + unicode_casefold("ÄÖÜ"));
println("normalized: " + string(unicode_is_normalized("café", "NFC")));
'''

MEDIA = r'''// 19. Media: Images & Audio
// A 2x2 RGB image
let pixels = bytes([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
let png = png_encode(2, 2, 3, pixels);
let img = png_decode(png).unwrap();
println("png: " + string(img["width"]) + "x" + string(img["height"]));

// A short 16-bit mono WAV
let wav = wav_encode(8000, 1, 16, bytes([0, 0, 0, 64]));
let snd = wav_decode(wav).unwrap();
println("wav: " + string(snd["sample_rate"]) + " Hz, " + string(snd["channels"]) + " channel(s)");
'''

TIME = r'''// 20. Date & Time
import "tz" as tz;

// Fixed epochs only: the sandbox has no wall clock
let p = datetime_parts(0);
println("epoch 0:   " + string(p["year"]) + "-" + string(p["month"]) + "-" + string(p["day"]));
println("formatted: " + format_datetime(0, "%Y-%m-%d %H:%M:%S"));

println("UTC:       " + tz_format("UTC", 0, "%Y-%m-%d %H:%M"));
println("zones:     " + string(len(tz_names()) > 100));
let info = tz_info("UTC", 0).unwrap();
println("tz offset: " + string(info["offset"]) + " seconds");
'''

INTROSPECTION = r'''// 21. Runtime Introspection
println(type_of(1) + "/" + type_of("s") + "/" + type_of(1.5) + "/" + type_of(true) + "/" + type_of(null));
println(type_of([1]) + "/" + type_of(map("a", 1)) + "/" + type_of(set(1)) + "/" + type_of(tuple(1)));

// Garbage collector stats
let keep = [1, 2, 3];
gc_collect();
println("live cells: " + string(gc_live_cells() > 0));

// Scheduler
println("task_count: " + string(task_count() >= 0));

// Assertions
assert(1 + 1 == 2);
assert_eq(2 + 2, 4);
println("asserts passed");
'''

HTTP_SIM = r'''// 22. Simulated Web Server Route Handler
class Request {
    public let method;
    public let path;
}

class Response {
    public let status;
    public let body;
}

fn handle_route(req) {
    let res = new Response();
    if (req.path == "/api/status") {
        res.status = 200;
        res.body = "{\"status\":\"ok\",\"platform\":\"wasm32\"}";
    } else {
        res.status = 404;
        res.body = "{\"error\":\"Not Found\"}";
    }
    return res;
}

let req = new Request();
req.method = "GET";
req.path = "/api/status";

println("Processing HTTP Request: " + req.method + " " + req.path);
let res = handle_route(req);
println("HTTP " + string(res.status) + " OK");
println("Payload: " + res.body);
'''

CALCULATOR = r'''// 23. Spid3r Calculator Engine State Machine Demo
println("=========================================");
println("  🧮 Spid3r Calculator Engine State Demo");
println("=========================================");

class Calculator {
    public let acc;
    public let op;

    fn new() {
        self.acc = 0;
        self.op = "+";
    }

    public fn compute(self, next_op, val) {
        if (self.op == "+") { self.acc = self.acc + val; }
        elif (self.op == "-") { self.acc = self.acc - val; }
        elif (self.op == "*") { self.acc = self.acc * val; }
        elif (self.op == "/") {
            if (val == 0) {
                println("Error: Division by 0");
                return self.acc;
            }
            self.acc = self.acc / val;
        }
        self.op = next_op;
        return self.acc;
    }
}

let calc = new Calculator();
println("Initial display:", calc.acc);

println("  Step 1: + 50   -> display:", calc.compute("+", 50));
println("  Step 2: * 4    -> display:", calc.compute("*", 4));
println("  Step 3: - 20   -> display:", calc.compute("-", 20));
println("  Step 4: / 2    -> display:", calc.compute("=", 2));

println("\nFinal Calculated Result:", calc.acc);
println("Desk calculator left-to-right chaining verified!");
'''

BENCHMARK = r'''// 24. Benchmark: 100,000 iterations
println("Running 100,000 loop iterations in browser WASM...");
let count = 0;
for let i = 0; i < 100000; i = i + 1 {
    count = count + 1;
}
println("Benchmark completed! Total:", count);
'''

STDLIB = r'''// 25. The standard library, inside WebAssembly
import "toml";
import "yaml" as yml;
import "semver" as sv;

println("=== Standard library in the browser ===");

// TOML parses AND writes; the module sources are compiled into the wasm.
    let cfg = toml_parse("name = \"spid3r\"\n[server]\nport = 8080\n");
println("name: " + string(cfg["name"]));
println("server.port: " + string(toml_get(cfg, "server.port")));
print(toml_write(cfg));

// YAML
    let doc = yml.yaml_parse("host: a.io\nports:\n  - 80\n  - 443\n");
println("yaml host: " + string(doc["host"]));
println("yaml port: " + string(doc["ports"][1]));

// Semver comparison
println("1.2.3 vs 1.10.0: " + string(sv.compare("1.2.3", "1.10.0")));

// Compression and hashing are native, so they run at full speed.
let text = "the quick brown fox jumps over the lazy dog. ";
let big = text + text + text + text;
let gz = gzip_compress(big);
println("raw " + string(len(big)) + " -> gzip " + string(len(gz)) + " bytes");
println("round trip ok: " + string(gunzip(gz).unwrap().to_string().unwrap() == big));
println("sha256(abc): " + sha256("abc"));
'''

PRESETS = [
    ("hello", "1. Hello World & System", HELLO),
    ("fib", "2. Fibonacci & Recursion", FIB),
    ("control", "3. Control Flow", CONTROL),
    ("functions", "4. Functions & Closures", FUNCTIONS),
    ("strings", "5. Strings & Interpolation", STRINGS),
    ("collections", "6. Lists, Maps & Sets", COLLECTIONS),
    ("datastructures", "7. Built-in Data Structures", DATASTRUCTURES),
    ("oop", "8. Classes & Inheritance", OOP),
    ("operators", "9. Operator Overloading", OPERATORS),
    ("enums", "10. Enums & Pattern Matching", ENUMS),
    ("errors", "11. Error Handling", ERRORS),
    ("concurrency", "12. Tasks & Channels", CONCURRENCY),
    ("typing", "13. Gradual Typing", TYPING),
    ("numeric", "14. Math & Numeric Types", NUMERIC),
    ("formats", "15. Serialization Formats", FORMATS),
    ("crypto", "16. Crypto & Encoding", CRYPTO),
    ("compression", "17. Compression", COMPRESSION),
    ("text", "18. Regex & Unicode", TEXT),
    ("media", "19. Images & Audio", MEDIA),
    ("time", "20. Date & Time", TIME),
    ("introspection", "21. Runtime Introspection", INTROSPECTION),
    ("http_sim", "22. Simulated Web Server Route", HTTP_SIM),
    ("calculator", "23. Calculator Engine Demo", CALCULATOR),
    ("benchmark", "24. Benchmark (100k Loops)", BENCHMARK),
    ("stdlib", "25. Standard Library & Compression", STDLIB),
]


def to_js_template(src: str) -> str:
    """Escape a Spid3r source for a JS template literal."""
    out = src.replace(BACKSLASH, BACKSLASH * 2).replace("`", "\\`").replace("${", "\\${")
    if "\r" in out:
        out = out.replace("\r\n", "\n").replace("\r", "\n")
    return out


def main() -> int:
    path = ROOT + "/docs/html/playground.html"
    with open(path, "r", encoding="utf-8") as f:
        html = f.read()

    start = html.index("const PRESETS = {")
    end_marker = "\n      };"
    end = html.index(end_marker, start)
    old_block = html[start:end + len(end_marker)]
    old_keys = re.findall(r"^\s{8}(\w+): `", old_block, re.M)
    print("replacing presets:", old_keys)

    lines = ["      const PRESETS = {"]
    for key, _label, src in PRESETS:
        body = to_js_template(src)
        lines.append("        %s: `%s`," % (key, body))
    lines.append("      };")
    new_block = "\n".join(lines)
    html = html[:start] + new_block + html[end + len(end_marker):]

    # Selector options
    opt_start = html.index('        <select class="select-input" id="preset-selector">')
    opt_end = html.index("        </select>", opt_start)
    opts = ['        <select class="select-input" id="preset-selector">']
    for key, label, _src in PRESETS:
        opts.append('          <option value="%s">%s</option>' % (key, label))
    opts.append("        </select>")
    html = html[:opt_start] + "\n".join(opts) + html[opt_end + len("        </select>"):]

    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(html)
    print("wrote %d presets" % len(PRESETS))
    return 0


if __name__ == "__main__":
    sys.exit(main())

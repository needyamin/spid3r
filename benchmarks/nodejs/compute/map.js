function buildTable(n) {
  const t = {};
  for (let i = 0; i < n; i++) t["k" + i] = (i * 37 + 11) % 997;
  return t;
}
const t = buildTable(64);
const t0 = process.hrtime.bigint();
let acc = 0;
for (let r = 0; r < 100000; r++) acc += t["k" + (r % 64)];
const us = Number((process.hrtime.bigint() - t0) / 1000n);
console.log("RESULT " + acc + " 100000");
console.log("TIME_US " + us);

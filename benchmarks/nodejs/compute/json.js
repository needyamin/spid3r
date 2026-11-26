function buildRows(n) {
  const rows = [];
  for (let i = 0; i < n; i++) {
    rows.push({id: i, sku: "SKU-" + i, qty: (i % 7) + 1, price: (i * 37) % 1000});
  }
  return rows;
}
const rows = buildRows(200);
const t0 = process.hrtime.bigint();
let total = 0, lastLen = 0;
for (let r = 0; r < 600; r++) {
  const s = JSON.stringify(rows);
  const back = JSON.parse(s);
  lastLen = s.length;
  for (let i = 0; i < back.length; i++) {
    const row = back[i];
    total += row.price * row.qty + row.id;
  }
}
const us = Number((process.hrtime.bigint() - t0) / 1000n);
console.log("RESULT " + total + " " + lastLen);
console.log("TIME_US " + us);

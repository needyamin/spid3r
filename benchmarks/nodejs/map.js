const n = 20000;
const m = new Map();
let i = 0;
while (i < n) {
  m.set("k" + String(i), i);
  i = i + 1;
}
let total = 0;
i = 0;
while (i < n) {
  total = total + m.get("k" + String(i));
  i = i + 1;
}
console.log(total);

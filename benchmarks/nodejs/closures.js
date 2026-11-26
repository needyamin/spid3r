const n = 20000;
let total = 0;
let i = 0;
while (i < n) {
  const captured = i;
  const adder = (x) => x + captured;
  total = total + adder(i);
  i = i + 1;
}
console.log(total);

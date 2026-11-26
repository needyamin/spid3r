const n = 100000;
const xs = [];
let i = 0;
while (i < n) {
  xs.push(i);
  i = i + 1;
}
let total = 0;
while (xs.length > 0) {
  total = total + xs.pop();
}
console.log(total);

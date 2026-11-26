class Counter {
  constructor() {
    this.n = 0;
  }
  bump() {
    this.n = this.n + 1;
    return this.n;
  }
}

const n = 200000;
const c = new Counter();
let i = 0;
let last = 0;
while (i < n) {
  last = c.bump();
  i = i + 1;
}
console.log(last);

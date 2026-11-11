function isPrime(n) {
  if (n < 2) return false;
  let d = 2;
  while (d * d <= n) { if (n % d === 0) return false; d += 1; }
  return true;
}
const t0 = process.hrtime.bigint();
let i = 2, count = 0, checksum = 0;
while (i < 50000) { if (isPrime(i)) { count += 1; checksum += i; } i += 1; }
const us = Number((process.hrtime.bigint() - t0) / 1000n);
console.log('RESULT ' + count + ' ' + checksum);
console.log('TIME_US ' + us);

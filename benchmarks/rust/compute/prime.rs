use std::time::Instant;
fn is_prime(n: i64) -> bool {
    if n < 2 { return false; }
    let mut d: i64 = 2;
    while d * d <= n { if n % d == 0 { return false; } d += 1; }
    true
}
fn main() {
    let t0 = Instant::now();
    let mut i: i64 = 2;
    let mut count: i64 = 0;
    let mut checksum: i64 = 0;
    while i < 50000 { if is_prime(i) { count += 1; checksum += i; } i += 1; }
    let us = t0.elapsed().as_micros();
    println!("RESULT {} {}", count, checksum);
    println!("TIME_US {}", us);
}

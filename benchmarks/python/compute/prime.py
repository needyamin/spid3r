import time
def is_prime(n):
    if n < 2: return False
    d = 2
    while d * d <= n:
        if n % d == 0: return False
        d += 1
    return True
t0 = time.perf_counter_ns()
i, count, checksum = 2, 0, 0
while i < 50000:
    if is_prime(i):
        count += 1; checksum += i
    i += 1
us = (time.perf_counter_ns() - t0) // 1000
print("RESULT", count, checksum)
print("TIME_US", us)

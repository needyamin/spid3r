import time
def build_table(n):
    return {"k" + str(i): (i * 37 + 11) % 997 for i in range(n)}
t = build_table(64)
t0 = time.perf_counter_ns()
acc = 0
for r in range(100000):
    acc += t["k" + str(r % 64)]
us = (time.perf_counter_ns() - t0) // 1000
print("RESULT", acc, 100000)
print("TIME_US", us)

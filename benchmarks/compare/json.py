import json, time
def build_rows(n):
    return [{"id": i, "sku": "SKU-" + str(i), "qty": (i % 7) + 1,
             "price": (i * 37) % 1000} for i in range(n)]
rows = build_rows(200)
t0 = time.perf_counter_ns()
total = 0
last_len = 0
for r in range(600):
    s = json.dumps(rows, separators=(",", ":"))
    back = json.loads(s)
    last_len = len(s)
    for row in back:
        total += row["price"] * row["qty"] + row["id"]
us = (time.perf_counter_ns() - t0) // 1000
print("RESULT", total, last_len)
print("TIME_US", us)

import queue
import threading

def main():
    n = 2000
    ch = queue.Queue()
    def worker():
        for i in range(n):
            ch.put(i * 2)
    t = threading.Thread(target=worker)
    t.start()
    total = 0
    for _ in range(n):
        total += ch.get()
    t.join()
    print(total)

if __name__ == "__main__":
    main()

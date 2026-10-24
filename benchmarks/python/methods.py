class Counter:
    def __init__(self):
        self.n = 0
    def bump(self):
        self.n = self.n + 1
        return self.n

def main():
    n = 200000
    c = Counter()
    i = 0
    last = 0
    while i < n:
        last = c.bump()
        i = i + 1
    print(last)

if __name__ == "__main__":
    main()

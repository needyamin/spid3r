def main():
    n = 20000
    total = 0
    i = 0
    while i < n:
        def make_adder(captured_i):
            return lambda x: x + captured_i
        adder = make_adder(i)
        total = total + adder(i)
        i = i + 1
    print(total)

if __name__ == "__main__":
    main()

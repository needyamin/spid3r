def main():
    n = 20000
    m = {}
    i = 0
    while i < n:
        m["k" + str(i)] = i
        i = i + 1
    total = 0
    i = 0
    while i < n:
        total = total + m["k" + str(i)]
        i = i + 1
    print(total)

if __name__ == "__main__":
    main()

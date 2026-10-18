def main():
    n = 100000
    xs = []
    i = 0
    while i < n:
        xs.append(i)
        i = i + 1
    total = 0
    while len(xs) > 0:
        total = total + xs.pop()
    print(total)

if __name__ == "__main__":
    main()

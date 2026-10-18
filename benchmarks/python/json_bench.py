import json

def build(i):
    return {"id": i, "name": "item-" + str(i), "tags": ["a", "b", "c"]}

def main():
    n = 2000
    count = 0
    i = 0
    while i < n:
        text = json.dumps(build(i))
        back = json.loads(text)
        count = count + back["id"]
        i = i + 1
    print(count)

if __name__ == "__main__":
    main()

function build(i) {
  return { id: i, name: "item-" + String(i), tags: ["a", "b", "c"] };
}

const n = 2000;
let count = 0;
let i = 0;
while (i < n) {
  const text = JSON.stringify(build(i));
  const back = JSON.parse(text);
  count = count + back.id;
  i = i + 1;
}
console.log(count);

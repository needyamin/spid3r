import os

pd = r'C:\Users\needyamin\OneDrive\Desktop\GaliLang\audit_notes\probes'

def list_prog(n):
    items = ', '.join(str(i) for i in range(1, n + 1))
    return 'fn main() {\n  let xs = [%s];\n  println(len(xs));\n  return 0;\n}\n' % items

for n in (256, 300):
    with open(os.path.join(pd, 'list%d.matrix' % n), 'w', encoding='utf-8') as f:
        f.write(list_prog(n))

def cap_prog(n):
    decls = '\n'.join('  let v%d = %d;' % (i, i) for i in range(1, n + 1))
    refs = ' + '.join('v%d' % i for i in range(1, n + 1))
    return ('fn main() {\n%s\n  let f = fn() { return %s; };\n  v1 = 1001;\n'
            '  println(f());\n  return 0;\n}\n' % (decls, refs))

for n in (128, 129, 130):
    with open(os.path.join(pd, 'cap%d.matrix' % n), 'w', encoding='utf-8') as f:
        f.write(cap_prog(n))

print('generated: list256 list300 cap128 cap129 cap130')

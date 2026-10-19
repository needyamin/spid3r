import io

p = r'C:\Users\needyamin\OneDrive\Desktop\GaliLang\src\semantic\mod.rs'
src = io.open(p, encoding='utf-8').read()
lines = src.split('\n')
sites = [682, 699, 710, 727, 791, 827, 867, 920, 1000, 1344]
for ln in sorted(sites, reverse=True):
    i = ln - 1
    text = lines[i]
    pos = text.index('VarInfo')
    start = text.index('{', pos)
    depth = 0
    j = i
    close_line = None
    close_col = None
    while j < len(lines):
        t = lines[j]
        s = start if j == i else 0
        for k in range(s, len(t)):
            ch = t[k]
            if ch == '{':
                depth += 1
            elif ch == '}':
                depth -= 1
                if depth == 0:
                    close_line, close_col = j, k
                    break
        if close_line is not None:
            break
        j += 1
    if close_line is None:
        raise SystemExit('no close found at ' + str(ln))
    t = lines[close_line]
    indent = t[:close_col]
    lines[close_line] = t[:close_col] + '    is_import: false,\n' + indent + '}' + t[close_col + 1:]
io.open(p, 'w', encoding='utf-8', newline='').write('\n'.join(lines))
print('inserted at', len(sites), 'sites')

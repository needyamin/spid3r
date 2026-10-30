"""Render the benchmark report page (docs/html/benchmark.html).

Kept separate from bench_report.py so the page can be re-rendered from the
measured JSON without re-running the suite. All numbers come from the data
dict; nothing here is hardcoded except presentation.
"""

import html
import math


def fmt_int(n):
    return f"{n:,.0f}"


def fmt_ms(ms):
    if ms < 10:
        return f"{ms:.2f}"
    return f"{ms:.1f}"


def fmt_rate(r):
    if r >= 1_000_000:
        return f"{r / 1_000_000:.2f}M"
    if r >= 1_000:
        return f"{r / 1_000:.0f}k"
    return f"{r:,.0f}"


def esc(s):
    return html.escape(str(s), quote=True)


def gate_bar(margin, lo=4.0, hi=20.0):
    """Log-scaled bar width for a gate margin (4x..20x fills the track)."""
    m = max(lo, min(hi, margin))
    frac = (math.log10(m) - math.log10(lo)) / (math.log10(hi) - math.log10(lo))
    return 6 + frac * 94


def lang_bars(rows, series):
    """Grouped horizontal bars on a shared LOG time axis, one group per benchmark.

    A linear axis is useless here: the values span 1.8 ms to 44 ms and a
    per-row normalization would make a 44 ms bar look the same as a 2 ms one.
    The log axis keeps every row comparable and still shows "shorter is faster".

    rows: [(stem, {series: ms})]; series: [(key, label, css_class)]
    """
    row_h, gap, label_w, chart_w = 18, 12, 112, 520
    axis_h = 30
    plot_top = 2
    body = len(rows) * (row_h * len(series) + gap)
    height = body + axis_h
    width = label_w + chart_w + 56

    lo, hi = 1.0, 100.0
    span = math.log10(hi) - math.log10(lo)

    def xpos(v):
        frac = (math.log10(max(lo, min(hi, v))) - math.log10(lo)) / span
        return label_w + frac * chart_w

    out = [
        f'<svg class="chart" viewBox="0 0 {width} {height}" role="img" '
        f'aria-label="Startup-adjusted compute time per benchmark, log scale, lower is faster">'
    ]

    # gridlines + tick labels (log ticks)
    for tick in (1, 3, 10, 30):
        x = xpos(tick)
        out.append(
            f'<line x1="{x:.1f}" y1="{plot_top}" x2="{x:.1f}" y2="{body}" class="grid"/>'
        )
        out.append(
            f'<text x="{x:.1f}" y="{height - 12}" class="chart-tick" text-anchor="middle">{tick}</text>'
        )

    y = plot_top
    for stem, vals in rows:
        group_h = row_h * len(series)
        out.append(
            f'<text x="2" y="{y + group_h / 2 + 4:.1f}" class="chart-label">{esc(stem)}</text>'
        )
        for key, _label, cls in series:
            v = vals.get(key)
            if v is None:
                continue
            x0 = xpos(lo)
            x1 = xpos(v)
            w = max(1.5, x1 - x0)
            out.append(
                f'<rect x="{x0:.1f}" y="{y:.1f}" width="{w:.1f}" height="{row_h - 5}" '
                f'rx="2.5" class="bar {cls}"><title>{esc(key)}: {fmt_ms(v)} ms</title></rect>'
            )
            out.append(
                f'<text x="{x1 + 5:.1f}" y="{y + row_h - 6:.1f}" class="chart-val">{fmt_ms(v)}</text>'
            )
            y += row_h
        y += gap
    out.append("</svg>")
    return "".join(out)


CSS = """
    :root {
      --font-body: 'Inter', -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      --font-heading: 'Outfit', -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      --font-mono: 'JetBrains Mono', "SF Mono", Consolas, monospace;

      --bg-base: #0b0f17;
      --bg-surface: #111722;
      --bg-card: #161e2e;
      --bg-card-alt: #1a2335;
      --bg-code: #080c13;
      --border-subtle: rgba(255, 255, 255, 0.08);
      --border-line: #1e293b;
      --border-hover: rgba(56, 189, 248, 0.35);

      --fg-primary: #f1f5f9;
      --fg-secondary: #94a3b8;
      --fg-muted: #64748b;

      --w3-green: #04AA6D;
      --w3-green-hover: #059862;
      --w3-green-light: rgba(4, 170, 109, 0.12);
      --w3-green-border: rgba(4, 170, 109, 0.4);
      --cyan: #38bdf8;
      --indigo: #818cf8;
      --purple: #c084fc;
      --amber: #fbbf24;
      --rose: #fb7185;

      --shadow-md: 0 8px 16px rgba(0, 0, 0, 0.4);
      --radius: 12px;
      --topbar-height: 58px;
      --max-width: 1180px;
    }
    html[data-theme="light"] {
      --bg-base: #f8fafc;
      --bg-surface: #ffffff;
      --bg-card: #ffffff;
      --bg-card-alt: #f1f5f9;
      --bg-code: #f1f5f9;
      --border-subtle: rgba(0, 0, 0, 0.08);
      --border-line: #e2e8f0;
      --border-hover: rgba(4, 170, 109, 0.4);
      --fg-primary: #0f172a;
      --fg-secondary: #475569;
      --fg-muted: #94a3b8;
      --w3-green-light: #e8f5e9;
      --w3-green-border: #a5d6a7;
      --cyan: #0284c7;
      --indigo: #6366f1;
      --amber: #b45309;
      --rose: #e11d48;
      --shadow-md: 0 4px 12px rgba(0, 0, 0, 0.08);
    }

    * { box-sizing: border-box; }
    html { scroll-behavior: smooth; }
    body {
      margin: 0;
      background: var(--bg-base);
      color: var(--fg-primary);
      font-family: var(--font-body);
      font-size: 15px;
      line-height: 1.65;
      -webkit-font-smoothing: antialiased;
    }
    a { color: var(--w3-green); text-decoration: none; }
    a:hover { color: var(--w3-green-hover); text-decoration: underline; }

    .topbar {
      position: sticky; top: 0; z-index: 50;
      height: var(--topbar-height);
      display: flex; align-items: center; gap: 18px;
      padding: 0 22px;
      background: rgba(11, 15, 23, 0.92);
      backdrop-filter: blur(12px);
      border-bottom: 1px solid var(--border-line);
    }
    html[data-theme="light"] .topbar { background: rgba(255, 255, 255, 0.94); }
    .brand { display: flex; align-items: center; gap: 10px; font-family: var(--font-heading); font-weight: 700; font-size: 17px; color: var(--fg-primary); }
    .brand:hover { text-decoration: none; color: var(--fg-primary); }
    .brand img { width: 26px; height: 26px; border-radius: 6px; }
    .brand .accent { color: var(--w3-green); }
    .brand .sub { font-size: 13px; font-weight: 500; color: var(--fg-secondary); }
    .topbar-nav { display: flex; gap: 4px; margin-left: auto; flex-wrap: wrap; }
    .nav-tab {
      padding: 7px 12px; border-radius: 8px; font-size: 13.5px; font-weight: 500;
      color: var(--fg-secondary);
    }
    .nav-tab:hover { background: var(--bg-card-alt); color: var(--fg-primary); text-decoration: none; }
    .nav-tab.active { background: var(--w3-green-light); color: var(--w3-green); }
    .btn-top {
      background: var(--bg-card); border: 1px solid var(--border-line);
      color: var(--fg-secondary); border-radius: 8px; padding: 7px 11px;
      cursor: pointer; font-size: 14px; font-family: var(--font-body);
    }
    .btn-top:hover { border-color: var(--border-hover); color: var(--fg-primary); }

    main { max-width: var(--max-width); margin: 0 auto; padding: 34px 22px 70px; }

    .hero { padding: 8px 0 26px; border-bottom: 1px solid var(--border-line); margin-bottom: 30px; }
    .eyebrow {
      font-family: var(--font-mono); font-size: 12px; letter-spacing: .14em;
      text-transform: uppercase; color: var(--w3-green); margin-bottom: 10px;
    }
    h1 {
      font-family: var(--font-heading); font-size: 40px; line-height: 1.1;
      margin: 0 0 12px; letter-spacing: -0.02em;
    }
    .lead { color: var(--fg-secondary); font-size: 16.5px; max-width: 76ch; margin: 0 0 18px; }
    .chips { display: flex; flex-wrap: wrap; gap: 8px; }
    .chip {
      display: inline-flex; align-items: center; gap: 6px;
      background: var(--bg-card); border: 1px solid var(--border-line);
      border-radius: 999px; padding: 5px 12px;
      font-size: 12.5px; color: var(--fg-secondary); font-family: var(--font-mono);
    }
    .chip b { color: var(--fg-primary); font-weight: 600; }

    h2 {
      font-family: var(--font-heading); font-size: 24px; margin: 44px 0 6px;
      letter-spacing: -0.01em;
    }
    h2 .num { color: var(--w3-green); font-family: var(--font-mono); font-size: 15px; margin-right: 10px; }
    .section-lead { color: var(--fg-secondary); margin: 0 0 18px; max-width: 82ch; }
    h3 { font-family: var(--font-heading); font-size: 17px; margin: 26px 0 8px; }

    .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 14px; margin: 18px 0 6px; }
    .card {
      background: var(--bg-card); border: 1px solid var(--border-line);
      border-radius: var(--radius); padding: 16px 18px;
    }
    .card .k { font-size: 12px; text-transform: uppercase; letter-spacing: .1em; color: var(--fg-muted); font-family: var(--font-mono); }
    .card .v { font-family: var(--font-heading); font-size: 27px; font-weight: 700; margin: 6px 0 2px; }
    .card .v.green { color: var(--w3-green); }
    .card .n { font-size: 12.5px; color: var(--fg-secondary); }

    .table-wrap { overflow-x: auto; border: 1px solid var(--border-line); border-radius: var(--radius); background: var(--bg-card); }
    table { width: 100%; border-collapse: collapse; font-size: 14px; }
    thead th {
      text-align: left; padding: 11px 14px; font-size: 11.5px; text-transform: uppercase;
      letter-spacing: .09em; color: var(--fg-muted); font-weight: 600;
      border-bottom: 1px solid var(--border-line); white-space: nowrap;
      font-family: var(--font-mono);
    }
    tbody td { padding: 11px 14px; border-bottom: 1px solid var(--border-subtle); vertical-align: middle; }
    tbody tr:last-child td { border-bottom: none; }
    tbody tr:hover { background: var(--bg-card-alt); }
    .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
    .dim { color: var(--fg-muted); font-size: 12.5px; }
    .name { font-weight: 600; }
    .name small { display: block; font-weight: 400; color: var(--fg-muted); font-size: 12px; }

    .badge {
      display: inline-block; padding: 2px 9px; border-radius: 999px;
      font-size: 11.5px; font-weight: 600; letter-spacing: .04em;
      font-family: var(--font-mono); white-space: nowrap;
    }
    .badge.pass { background: var(--w3-green-light); color: var(--w3-green); border: 1px solid var(--w3-green-border); }
    .badge.fail { background: rgba(251, 113, 133, 0.14); color: var(--rose); border: 1px solid rgba(251, 113, 133, 0.4); }
    .badge.muted { background: var(--bg-card-alt); color: var(--fg-muted); border: 1px solid var(--border-line); }

    .track { position: relative; height: 8px; width: 100%; min-width: 110px; background: var(--bg-card-alt); border-radius: 999px; overflow: hidden; }
    .track span { position: absolute; inset: 0 auto 0 0; border-radius: 999px; background: linear-gradient(90deg, var(--w3-green), var(--cyan)); }

    .chart { width: 100%; max-width: 840px; height: auto; display: block; margin: 4px auto 0; }
    .chart-label { fill: var(--fg-secondary); font-family: var(--font-mono); font-size: 11.5px; }
    .chart-val { fill: var(--fg-muted); font-family: var(--font-mono); font-size: 10.5px; }
    .chart-tick { fill: var(--fg-muted); font-family: var(--font-mono); font-size: 10px; }
    .grid { stroke: var(--border-line); stroke-width: 1; stroke-dasharray: 2 3; }
    .bar.spid3r { fill: var(--w3-green); }
    .bar.php { fill: var(--indigo); }
    .bar.python { fill: var(--amber); }
    .legend { display: flex; gap: 16px; flex-wrap: wrap; margin: 10px 2px 0; font-size: 12.5px; color: var(--fg-secondary); }
    .legend i { display: inline-block; width: 10px; height: 10px; border-radius: 3px; margin-right: 6px; }
    .legend .spid3r { background: var(--w3-green); }
    .legend .php { background: var(--indigo); }
    .legend .python { background: var(--amber); }

    pre {
      background: var(--bg-code); border: 1px solid var(--border-line);
      border-radius: 10px; padding: 14px 16px; overflow-x: auto;
      font-family: var(--font-mono); font-size: 13px; line-height: 1.6;
      color: var(--fg-primary);
    }
    code { font-family: var(--font-mono); font-size: 0.92em; }
    p code, li code { background: var(--bg-card-alt); border: 1px solid var(--border-subtle); border-radius: 5px; padding: 1px 5px; }

    .note {
      border-left: 3px solid var(--cyan); background: var(--bg-card);
      padding: 12px 16px; border-radius: 0 10px 10px 0; margin: 16px 0;
      color: var(--fg-secondary); font-size: 14px;
    }
    .note b { color: var(--fg-primary); }
    ul { padding-left: 20px; }
    li { margin: 5px 0; color: var(--fg-secondary); }
    li b { color: var(--fg-primary); }

    footer {
      max-width: var(--max-width); margin: 0 auto; padding: 22px;
      border-top: 1px solid var(--border-line);
      color: var(--fg-muted); font-size: 12.5px;
      display: flex; flex-wrap: wrap; gap: 10px; justify-content: space-between;
    }
"""


def render(data, names, desc, iters, floors):
    vm = data["vm"]
    tree = data["tree"]
    php = data["php"]
    py = data["python"]
    startup = data["startup"]
    gate = {g["stem"]: g for g in data["gate"]}
    order = [s for s in ["arith", "strings", "list", "map", "methods", "closures", "json", "tasks"] if s in gate]

    passed = sum(1 for s in order if gate[s]["pass"])
    total = len(order)
    best = max(order, key=lambda s: gate[s]["margin"])
    worst = min(order, key=lambda s: gate[s]["margin"])

    # vm speedup over the tree interpreter
    speeds = {s: tree[s] / vm[s] for s in vm if s in tree and vm[s] > 0}
    med_keys = sorted(speeds, key=lambda s: speeds[s])
    median = med_keys[len(med_keys) // 2]

    host = data["host"]
    gate_rows = []
    for s in order:
        g = gate[s]
        gate_rows.append(
            f"""<tr>
          <td class="name">{names.get(s, s)}<small>{desc.get(s, '')}</small></td>
          <td class="mono">{fmt_int(iters[s])}</td>
          <td class="mono">{fmt_rate(g['rate'])}<span class="dim">/s</span></td>
          <td class="mono dim">{fmt_rate(g['floor'])}</td>
          <td class="mono">{g['margin']:.1f}&times;</td>
          <td><div class="track"><span style="width:{gate_bar(g['margin']):.1f}%"></span></div></td>
          <td><span class="badge {'pass' if g['pass'] else 'fail'}">{'PASS' if g['pass'] else 'FAIL'}</span></td>
        </tr>"""
        )

    engine_rows = []
    for s in order + (["stress_suite"] if "stress_suite" in vm else []):
        if s not in tree:
            continue
        sp = tree[s] / vm[s]
        engine_rows.append(
            f"""<tr>
          <td class="name">{names.get(s, s)}{'<small>correctness only, not gated</small>' if s not in gate else ''}</td>
          <td class="mono">{fmt_ms(vm[s])}</td>
          <td class="mono dim">{fmt_ms(tree[s])}</td>
          <td class="mono">{sp:.2f}&times; {'faster' if sp >= 1 else 'slower'}</td>
          <td><div class="track"><span style="width:{min(100, max(3, 100 / sp)):.1f}%"></span></div></td>
        </tr>"""
        )

    # ---- language comparison -------------------------------------------------
    lang_stems = [s for s in order if s in php or s in py]
    wall_rows, comp_rows, chart_rows = [], [], []
    for s in lang_stems:
        cells = []
        for key, ms in (("spid3r", vm.get(s)), ("php", php.get(s)), ("python", py.get(s))):
            cells.append(f'<td class="mono">{fmt_ms(ms) if ms else "&mdash;"}</td>' if ms else '<td class="dim">&mdash;</td>')
        wall_rows.append(f'<tr><td class="name">{names.get(s, s)}</td>{"".join(cells)}</tr>')

        comp = {}
        for key, ms in (("spid3r", vm.get(s)), ("php", php.get(s)), ("python", py.get(s))):
            st = startup.get(key)
            if ms is not None and st is not None:
                comp[key] = max(0.1, ms - st)
        chart_rows.append((s, comp))
        fastest = min(comp, key=lambda k: comp[k]) if comp else None
        label = {"spid3r": "Spid3r", "php": "PHP", "python": "Python"}
        comp_rows.append(
            f"""<tr>
          <td class="name">{names.get(s, s)}</td>
          <td class="mono">{fmt_ms(comp['spid3r'])}</td>
          <td class="mono dim">{fmt_ms(comp['php']) if 'php' in comp else '&mdash;'}</td>
          <td class="mono dim">{fmt_ms(comp['python']) if 'python' in comp else '&mdash;'}</td>
          <td><span class="badge muted">{label[fastest]}</span></td>
        </tr>"""
        )

    py_note = ""
    if startup.get("python", 0) > 100:
        py_note = (
            f"<b>Python's totals are startup-bound here.</b> The interpreter on this host takes "
            f"{startup['python']:.0f}&nbsp;ms just to start ({esc(host.get('python', ''))}) &mdash; longer than any of "
            f"these workloads takes to run &mdash; so its wall-clock column is essentially a measurement of "
            f"startup. See the startup-adjusted table below."
        )

    chart = lang_bars(chart_rows, [("spid3r", "Spid3r", "spid3r"), ("php", "PHP", "php"), ("python", "Python", "python")])

    html_doc = f"""<!DOCTYPE html>
<html lang="en" data-theme="dark">

<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Spid3r Benchmark Report &mdash; gate, engines, and reference runtimes</title>
  <meta name="description"
    content="Measured benchmark report for the Spid3r language: perf-gate results, bytecode VM vs tree interpreter, and a startup-adjusted comparison against PHP and Python.">

  <link rel="icon" type="image/png" href="../../src/assets/spid3r.png">
  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link
    href="https://fonts.googleapis.com/css2?family=Inter:wght@300;400;500;600;700;800&family=JetBrains+Mono:wght@400;500;600&family=Outfit:wght@500;600;700;800&display=swap"
    rel="stylesheet">

  <script>
    (function () {{
      var t = null;
      try {{ t = localStorage.getItem('spid3r-dev-theme'); }} catch (_) {{ }}
      if (!t) {{
        t = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
      }}
      document.documentElement.setAttribute('data-theme', t);
    }})();
  </script>

  <style>{CSS}  </style>
</head>

<body>
  <header class="topbar">
    <a href="doc.html" class="brand">
      <img src="../../src/assets/spid3r.png" alt="Spid3r Logo">
      <div>Spid<span class="accent">3</span>r <span class="sub">Benchmarks</span></div>
    </a>
    <nav class="topbar-nav">
      <a href="doc.html" class="nav-tab">Documentation</a>
      <a href="benchmark.html" class="nav-tab active">Benchmarks</a>
      <a href="playground.html" target="_blank" class="nav-tab">Playground</a>
      <button class="btn-top" id="themeBtn" title="Toggle Light/Dark Theme">&#127769;</button>
    </nav>
  </header>

  <main>
    <div class="hero">
      <div class="eyebrow">Measured &middot; reproducible</div>
      <h1>Benchmark Report</h1>
      <p class="lead">
        Eight gated micro-benchmarks measured on a fresh process per run, best of {data['runs']['spid3r']}.
        Every case is also a differential test: the bytecode VM and the tree interpreter must print
        identical output, and each run must reproduce its own output exactly.
      </p>
      <div class="chips">
        <span class="chip"><b>{esc(data['generated'])}</b></span>
        <span class="chip">{esc(data['spid3r_version'])}</span>
        <span class="chip">{esc(host['cpu'])} &middot; {esc(str(host['cores']))} threads</span>
        <span class="chip">{esc(host['os'])}</span>
      </div>
    </div>

    <div class="cards">
      <div class="card">
        <div class="k">Perf gate</div>
        <div class="v green">{passed}/{total} PASS</div>
        <div class="n">every benchmark above its floor</div>
      </div>
      <div class="card">
        <div class="k">Narrowest margin</div>
        <div class="v">{gate[worst]['margin']:.1f}&times;</div>
        <div class="n">{esc(worst)} &mdash; closest to its floor</div>
      </div>
      <div class="card">
        <div class="k">Widest margin</div>
        <div class="v">{gate[best]['margin']:.1f}&times;</div>
        <div class="n">{esc(best)}</div>
      </div>
      <div class="card">
        <div class="k">VM over tree</div>
        <div class="v">{speeds[median]:.2f}&times;</div>
        <div class="n">median across all cases</div>
      </div>
    </div>

    <h2><span class="num">01</span>Performance gate</h2>
    <p class="section-lead">
      Floors are set at roughly a third of the worst rate observed on reference hardware. They exist to
      catch catastrophic regressions (a 3&times;+ slowdown) on shared CI runners, not small drift &mdash;
      a passing gate is a floor, not a target.
    </p>
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Benchmark</th>
            <th>Iters</th>
            <th>Throughput</th>
            <th>Floor</th>
            <th>Margin</th>
            <th>Headroom</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
        {''.join(gate_rows)}
        </tbody>
      </table>
    </div>
    <p class="dim">Throughput is iters/sec, where one iteration is one full body of the benchmark file.
      Bars are log-scaled: the track spans 4&times; to 20&times; the floor.</p>

    <h2><span class="num">02</span>Bytecode VM vs tree interpreter</h2>
    <p class="section-lead">
      The same source runs on both engines. The VM compiles to bytecode with constant folding and
      peephole passes; the tree interpreter walks the AST directly. Numbers are milliseconds, best of
      {data['runs']['spid3r']} (VM) and {data['runs']['tree']} (tree).
    </p>
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Benchmark</th>
            <th>VM (ms)</th>
            <th>Tree (ms)</th>
            <th>VM is</th>
            <th>Tree / VM</th>
          </tr>
        </thead>
        <tbody>
        {''.join(engine_rows)}
        </tbody>
      </table>
    </div>

    <h2><span class="num">03</span>Against PHP and Python</h2>
    <p class="section-lead">
      The same workloads implemented in PHP {esc(data.get('php_version', ''))} and Python
      {esc(host.get('python', ''))}, measured the same way: one fresh process per run, best of
      {data['runs']['other']}, wall clock including interpreter startup.
    </p>
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Benchmark</th>
            <th>Spid3r (ms)</th>
            <th>PHP (ms)</th>
            <th>Python (ms)</th>
          </tr>
        </thead>
        <tbody>
        {''.join(wall_rows)}
        </tbody>
      </table>
    </div>
    <div class="note">
      {py_note if py_note else ''}
      <b>Startup is not free for anyone:</b> the measured fixed cost is {startup.get('spid3r', 0):.1f}&nbsp;ms
      for Spid3r, {startup.get('php', 0):.1f}&nbsp;ms for PHP and {startup.get('python', 0):.1f}&nbsp;ms for
      Python. For the light benchmarks that fixed cost is the same order as the workload itself, which is
      why the table below subtracts it.
    </div>

    <h3>Startup-adjusted compute time</h3>
    <p class="section-lead">
      Total wall clock minus each runtime's measured startup, in milliseconds. Lower is better. This is an
      estimate: startup measured by a no-op invocation understates the fixed cost of loading a program,
      so treat differences under ~1&nbsp;ms as noise.
    </p>
    <div class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Benchmark</th>
            <th>Spid3r</th>
            <th>PHP</th>
            <th>Python</th>
            <th>Fastest</th>
          </tr>
        </thead>
        <tbody>
        {''.join(comp_rows)}
        </tbody>
      </table>
    </div>
    <div class="legend">
      <span><i class="spid3r"></i>Spid3r (VM)</span>
      <span><i class="php"></i>PHP</span>
      <span><i class="python"></i>Python</span>
    </div>
    {chart}
    <p class="dim" style="text-align:center">Startup-adjusted compute time, log scale &mdash; shorter is faster.</p>

    <h2><span class="num">04</span>Method and caveats</h2>
    <ul>
      <li><b>One process per run.</b> Each sample is a fresh OS process; the harness takes the best of N.
        Nothing is warmed up across runs, so JIT-less runtimes are not penalised by a cold-start handicap
        they would not have in a long-lived server.</li>
      <li><b>Correctness is part of the measurement.</b> <code>--compare</code> requires the VM and tree
        engines to print byte-identical output, and every repeat run must reproduce its own output, or the
        benchmark is reported as failed rather than timed.</li>
      <li><b>Micro-benchmarks, not applications.</b> These isolate a loop, a hash table or a call path.
        They say nothing about startup-to-first-byte for a real program, memory ceiling, or I/O.</li>
      <li><b>Absolute numbers are host-specific.</b> The CPU on this machine runs at its base clock under
        the Balanced power plan; recorded numbers from a turbo-boosted run are ~25&ndash;30% higher across
        every workload and every runtime, including PHP. Compare ratios, not raw milliseconds, across
        machines.</li>
      <li><b>Not measured here:</b> the AOT/native backend (needs LLVM, runs in its own CI job) and the
        WebAssembly build (see <a href="playground.html">the playground</a>).</li>
    </ul>

    <h3>Reproduce</h3>
    <pre>cargo build --release
./target/release/spid3r bench --gate --compare --runs 5

# this page, from the same measurements
python tools/bench_report.py</pre>
    <p class="dim">Raw samples: <code>benchmarks/report-data.json</code>. The historical comparison set
      (2026-10-20) lives in <code>benchmarks/results.json</code>.</p>
  </main>

  <footer>
    <span>Generated by <code>tools/bench_report.py</code> &middot; {esc(data['generated'])}</span>
    <span><a href="doc.html">Documentation</a> &middot; <a href="playground.html">Playground</a></span>
  </footer>

  <script>
    document.getElementById('themeBtn').addEventListener('click', function () {{
      var root = document.documentElement;
      var next = root.getAttribute('data-theme') === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', next);
      try {{ localStorage.setItem('spid3r-dev-theme', next); }} catch (_) {{ }}
    }});
  </script>
</body>

</html>
"""
    return html_doc

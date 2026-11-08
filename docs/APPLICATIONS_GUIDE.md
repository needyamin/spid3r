# Spid3r Native GUI Application Showcase

This document details a complete, production-grade application written in the **Spid3r** programming language, utilizing its first-party standard library (`stats.matrix`, `ml.matrix`, and the native GUI subsystem).

---

## Native GUI Application: DataLab & Analytics Studio

- **Directory**: `examples/gui/data_studio/`
- **Source Script**: [`app.matrix`](../examples/gui/data_studio/app.matrix)
- **UI Frontend**: [`index.html`](../examples/gui/data_studio/index.html)
- **Batch Launcher**: [`run_gui.bat`](../examples/gui/data_studio/run_gui.bat)

### Architecture
- Runs on the Spid3r Tree Interpreter with Tao window and Wry WebView bridge (`spid3r gui <file.matrix> --html <index.html>`).
- Bidirectional event bridge:
  - Frontend emits user interaction events: `window.spid3r.emit(event, payload)`.
  - Spid3r registers native event listeners: `gui_on("compute_stats", on_compute_stats)`, `gui_on("regression", on_regression)`, `gui_on("matrix_op", on_matrix_op)`.
  - Spid3r executes native statistical and machine learning algorithms and pushes live state updates to the UI via `gui_eval(...)`.

### Features
1. **Statistical Analyzer**: Computes sample size, mean ($\mu$), median, variance ($\sigma^2$), standard deviation ($\sigma$), quartiles, and IQR using `stdlib/stats.matrix`. Renders an interactive histogram on Canvas.
2. **Machine Learning Linear Regression**: Performs closed-form normal equation solving using `stdlib/ml.matrix`. Fits slope, intercept, computes $R^2$ score, dynamically plots regression line on Canvas, and provides a custom coordinate predictor.
3. **Matrix 2x2 Decomposition**: Computes determinant, trace, eigenvalues, and matrix inversion using Spid3r math kernels.

### Running the GUI App
```powershell
# Interactive desktop window
.\target\debug\spid3r.exe gui examples\gui\data_studio\app.matrix --html examples\gui\data_studio\index.html

# Deterministic headless verification (no window)
.\target\debug\spid3r.exe gui examples\gui\data_studio\app.matrix --html examples\gui\data_studio\index.html --dry-run
```

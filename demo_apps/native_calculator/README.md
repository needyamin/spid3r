# Spid3r Native Calculator

A high-precision native terminal and CLI calculator written in pure Spid3r.

## Features

- **Direct Expression Evaluation**: Pass expressions directly as CLI arguments.
- **Interactive REPL Mode**: Run standalone terminal calculator session with history, clear, and recall.
- **Standard & Scientific Operations**: `+`, `-`, `*`, `/`, `%`, `neg`, `sqrt`, `sqr`, `inv`.
- **Zero Dependencies**: Runs on both the Spid3r Bytecode VM and the Tree Interpreter.

## Usage

### 1. Interactive Terminal Session
```bash
spid3r run demo_apps/native_calculator/calculator.matrix
```
Example interactive session:
```text
calc> 25 + 75
Result:  100

calc> * 2
Result:  200

calc> sqrt
Result:  14.1421356237

calc> history
History (3 entries):
  [0] 25 + 75 = 100
  [1] 100 × 2 = 200
  [2] sqrt(200) = 14.1421356237

calc> q
Goodbye!
```

### 2. Direct CLI Evaluation
```bash
spid3r run demo_apps/native_calculator/calculator.matrix 25 + 75
# Output: 100
```

### 3. Self-Test Suite
```bash
spid3r run demo_apps/native_calculator/calculator.matrix test
```

### 4. Bytecode Compilation
```bash
spid3r compile demo_apps/native_calculator/calculator.matrix -o calculator.matrixc
spid3r exec calculator.matrixc test
```

# Spid3r Examples Directory

A curated collection of runnable sample programs, native GUI applications, multi-threaded web servers, and flagship demos written in Spid3r.

---

## 📂 Subdirectories

| Directory | Description | Readme / Guide |
| :--- | :--- | :--- |
| [gui/](gui/) | **Desktop GUI Apps**: Native window apps with Tao/Wry webview and bidirectional IPC bridge. Contains `counter.matrix` and the `data_studio/` analytics suite. | [gui/README.md](gui/README.md) |
| [web/](web/) | **Web & HTTP Servers**: Multi-threaded REST APIs and web servers using `import "http"` and `import "web"`. Contains `server.matrix` and `cloudops_hub/`. | [web/README.md](web/README.md) |
| [demo_apps/](demo_apps/) | **Flagship Demo Applications**: Full-featured desktop native calculator (`calculator.exe`) and web calculator. | [demo_apps/native_calculator/README.md](demo_apps/native_calculator/README.md) |

---

## 📜 Core Language Examples

Every `.matrix` file in this directory demonstrates a specific language capability and can be run directly with `spid3r run <filename>` (with the exception of `math_util.matrix`, which is a library module imported by `modules.matrix`):

| File | Topic | Description |
| :--- | :--- | :--- |
| `hello.matrix` | Basics | Minimal "Hello, World!" program. |
| `variables.matrix` | Types & Variables | Variable declarations, scoping, and type inference. |
| `conditions.matrix` | Control Flow | `if`, `elif`, and `else` conditional expressions. |
| `loops.matrix` | Iteration | `for`, `while`, loop ranges, and `break`/`continue`. |
| `functions.matrix` | Functions | First-class functions, closures, recursion, and return values. |
| `higher_order.matrix` | Functional | Map, filter, reduce, and higher-order function composition. |
| `class.matrix` | OOP | Classes, constructors (`fn new()`), fields, and methods. |
| `interface.matrix` | Interfaces | Defining interfaces and implementing them (`implements`). |
| `error_handling.matrix` | Exceptions | `try`, `catch`, `finally`, `throw`, and `ok`/`err` results. |
| `tasks.matrix` | Concurrency | Asynchronous task spawning, parallelism, and task join. |
| `channel.matrix` | Channels | CSP-style channel communication between concurrent tasks. |
| `modules.matrix` | Modularization | Code organization, file imports, and namespace aliasing. |
| `math_util.matrix` | Modules | Shared utility functions module imported by other examples (library module; no `fn main()`). |
| `data_science_pipeline.matrix` | Data Science | Statistical analysis, matrix math, and data pipelines. |
| `server_report.matrix` | Scripting | Automation script generating server metric summaries. |
| `web_service.matrix` | Services | Networking simulation and structured payload dispatch. |

### Data Structures (`docs/DATA_STRUCTURES.md`)
| File | Collection Type |
| :--- | :--- |
| `list.matrix` | Dynamic array / list |
| `map.matrix` | Hash map / dictionary |
| `set.matrix` | Hash set |
| `stack.matrix` | LIFO stack |
| `queue.matrix` | FIFO queue |
| `linked_list.matrix` | Singly linked list |
| `bst.matrix` | Binary search tree |
| `heap.matrix` | Priority queue / binary heap |
| `graph.matrix` | Directed / undirected graph |

---

## ⚡ Running Examples

Run any flat example using the default interpreter:
```bash
spid3r run examples/hello.matrix
```

Run on the bytecode virtual machine:
```bash
spid3r run --engine vm examples/hello.matrix
```

Run the entire example test suite:
```bash
spid3r test
```

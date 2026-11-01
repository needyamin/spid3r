# Spid3r Mini E-Commerce Platform

A production-grade, full-stack demonstration application showing how to build real-world software using the **Spid3r programming language** with native **SQLite 3** persistence, zero external backend dependencies, and a modern **Bootstrap 5.3.3 + jQuery 3.7.1** frontend.

---

## 🌟 Highlights & Features

- **Pure Spid3r HTTP Server**: Uses `import "http";` (`HttpServer`) to serve REST API endpoints and static assets over real OS threads.
- **SQLite 3 Persistence**: Persistent transactional database using Spid3r's native `db_open("data/ecommerce.db")` C-loader (`sqlite3.dll`).
- **Zero Deadlocks**: Concurrent, non-blocking database queries without mutex deadlock pitfalls on add-to-cart operations.
- **Modern Bootstrap 5 & jQuery Frontend**: Clean, responsive storefront interface utilizing Bootstrap 5 grid, offcanvas drawers, modals, badges, and jQuery AJAX event binding (zero Vanilla JS DOM manipulation).
- **Two-Phase Transactional Checkout**: Explicit stock verification guards followed by atomic inventory deductions and order receipt creation with `ok(...)` / `err(...)` Result types.
- **Discount & Pricing Engine**: Dynamic promotional codes (`SPID3R20`, `BOOTSTRAP`, `SAVE10`, `FLASH50`), free shipping rules (orders over $50), and state sales tax computation.
- **Dual-Mode Architecture**:
  - **Live Server Mode**: Connects directly to the Spid3r HTTP backend on `http://127.0.0.1:8080`.
  - **Standalone In-Browser Mode**: Opening `public/index.html` directly in any web browser (`file:///...`) falls back to an interactive client-side simulation engine with identical business logic.

---

## 📁 Organized Project Structure

```
examples/demo_apps/ecommerce/
├── spider.toml                 # SPM package manifest (entry = "src/server.matrix")
├── README.md                   # Project documentation and architectural guide
├── start_server.bat            # 1-click Windows quick launcher
├── run.sh                      # 1-click Linux/macOS launcher
│
├── bin/                        # Native dynamic libraries
│   └── sqlite3.dll             # SQLite 3 native engine loaded by Spid3r
│
├── data/                       # Persistent database storage
│   ├── ecommerce.db            # SQLite 3 database file
│   ├── ecommerce.db-shm        # Shared memory WAL index
│   └── ecommerce.db-wal        # Write-Ahead Log journal
│
├── public/                     # Web storefront static assets
│   ├── index.html              # Clean HTML UI layout (Bootstrap 5 components)
│   ├── css/
│   │   └── app.css             # Custom styles, dark theme, and card animations
│   └── js/
│       └── app.js              # jQuery 3.7.1 frontend application logic
│
├── src/                        # Spid3r backend source code
│   ├── server.matrix           # HTTP REST API server & static asset delivery
│   └── main.matrix             # Standalone CLI suite & headless store engine
│
└── tests/                      # Automated test suites
    └── test_ecommerce.matrix   # Test runner verifying stock guards & cart logic
```

---

## 🚀 Quick Start

### 1. Run the HTTP Server & Web Storefront
From the project root:

```powershell
# Windows (One-Click)
.\start_server.bat

# or via command line from repo root:
spid3r run examples/demo_apps/ecommerce/src/server.matrix 8080

# Linux / macOS
./examples/demo_apps/ecommerce/run.sh
```

Then open your browser to:
👉 **`http://127.0.0.1:8080`**

### 2. Run the Automated Test Suite
You can execute the self-contained business logic test runner directly:

```bash
spid3r run examples/demo_apps/ecommerce/tests/test_ecommerce.matrix
```

---

## 📡 REST API Endpoints

| Method | Route | Description |
|---|---|---|
| `GET` | `/` | Serves the Bootstrap 5 web storefront (`public/index.html`) |
| `GET` | `/css/app.css` | Serves custom CSS stylesheet (`text/css`) |
| `GET` | `/js/app.js` | Serves jQuery client logic (`application/javascript`) |
| `GET` | `/api/health` | Server uptime, metrics, and SQLite status |
| `GET` | `/api/db/stats` | Detailed SQLite database table counts and storage metrics |
| `GET` | `/api/products` | Lists all products with stock and pricing from SQLite |
| `GET` | `/api/products/:id` | Returns single product details by SKU ID |
| `GET` | `/api/cart` | Returns active cart with subtotal, tax, and total |
| `POST` | `/api/cart/add` | Adds product `{ product_id, qty }` to cart (safe & deadlock-free) |
| `POST` | `/api/cart/update` | Updates quantity `{ product_id, qty }` |
| `POST` | `/api/cart/remove` | Removes product `{ product_id }` from cart |
| `POST` | `/api/cart/clear` | Clears all items from cart |
| `POST` | `/api/cart/coupon` | Applies promo code `{ coupon }` |
| `POST` | `/api/checkout` | Validates stock, deducts inventory, and records order in SQLite |
| `GET` | `/api/orders` | Retrieves completed order history from SQLite |
| `POST` | `/api/inventory/restock` | Replenishes all catalog SKU stock |

---

## 🎟️ Promotional Coupons Included

- **`SPID3R20`**: 20% discount on entire cart subtotal.
- **`BOOTSTRAP`**: Flat $15 discount.
- **`SAVE10`**: Flat $10 discount.
- **`FLASH50`**: 50% flash discount.

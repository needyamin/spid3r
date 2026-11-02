# Investigation Report: QueryBuilder Joins & Concurrency Hardening (M2-2)

**Author**: `teamwork_explorer` (M2-2)  
**Date**: 2026-11-01T23:56:00Z  
**Target Subsystem**: `framework/Database/DB.matrix`, `tests/test_query_builder.matrix`  
**Working Directory**: `C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web\.agents\teamwork\explorer_m2_2`  

---

## 1. Observation

### 1.1 Unimplemented `QueryBuilder.joins` in `framework/Database/DB.matrix`
Direct inspection of `framework/Database/DB.matrix` (lines 4–115) reveals:

```matrix
4: public class QueryBuilder {
5:     public let table_name;
6:     public let db_manager;
7:     public let select_cols;
8:     public let wheres;
9:     public let joins;
10:     public let order_col;
11:     public let order_dir;
12:     public let limit_count;
13:     public let offset_count;
14: 
15:     fn new(table: string, manager = null) {
16:         self.table_name = table;
17:         self.db_manager = manager;
18:         self.select_cols = list("*");
19:         self.wheres = list();
20:         self.joins = list();
21:         self.order_col = "";
22:         self.order_dir = "ASC";
23:         self.limit_count = -1;
24:         self.offset_count = 0;
25:     }
```

In lines 27–80:
- Only `select`, `where`, `orWhere`, `orderBy`, `limit`, and `offset` methods are defined.
- **Zero join methods** (`join`, `innerJoin`, `leftJoin`, `rightJoin`, `crossJoin`) exist on `QueryBuilder`.

In `build_select_sql(self)` (lines 85–114):
```matrix
85:     fn build_select_sql(self) {
86:         let sql = "SELECT " + self.select_cols.join(", ") + " FROM " + self.table_name;
87:         let params = list();
88: 
89:         if self.wheres.len() > 0 {
90:             sql = sql + " WHERE ";
91:             let first = true;
92:             for w in self.wheres {
...
113:         return map("sql", sql, "params", params);
114:     }
```
- `self.joins` is **completely ignored** during SQL compilation in `build_select_sql()`.
- `build_select_sql()` is not declared `public`, making SQL compilation non-inspectable from test code without executing queries against SQLite.
- `count()` (lines 143–166) also ignores `self.joins`, meaning `WHERE` conditions referencing joined tables fail with `no such column` when `count()` is called.

### 1.2 Missing `PRAGMA busy_timeout` in `DatabaseManager.connection()`
Direct inspection of `framework/Database/DB.matrix` (lines 283–290):

```matrix
283:     public fn connection(self) {
284:         if self.db_handle == null {
285:             // Ensure directory exists if needed
286:             self.db_handle = db_open(self.db_path);
287:             self.db_handle.exec("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;");
288:         }
289:         return self.db_handle;
290:     }
```

- SQLite's default busy timeout is `0` ms (immediate failure with `SQLITE_BUSY` when any file lock contention occurs).
- On Windows, file-locking semantics in write-ahead log (WAL) mode can trigger transient `database is locked` errors during rapid test cycles, multiple processes, or background CLI tasks.
- Spid3r's native SQLite driver (`src/stdlib/db.rs` line 544) handles multi-statement SQL strings in `.exec(...)` via `prepare_v2` and `tail` iteration. Executing `PRAGMA busy_timeout = 5000;` on connection setup instructs SQLite to retry internally for up to 5 seconds before returning `SQLITE_BUSY`.

### 1.3 Baseline Test Status
Execution of the test suites yields:
1. `Set-Location "examples/spid3r_web"; spid3r.exe magic test`: **6/6 test suites passed** (100% pass on baseline).
2. `Set-Location "examples/spid3r_web"; spid3r.exe run tests/run_all.matrix`: **6/6 test suites passed**.
3. `cargo test --test magic_tests`: **2 passed, 0 failed**.
4. `cargo test --test web_stack_tests`: **8 passed, 0 failed**.
5. `tests/test_query_builder.matrix` only contains a single test `test_query_builder_crud()` (61 lines) verifying basic CRUD without any joins or concurrency validation.

---

## 2. Logic Chain

1. **QueryBuilder Join Method Ergonomics**:
   - In `QueryBuilder.where(column, op, val = null)`, the builder supports both 2-argument shorthand (`where("id", 5)`) and 3-argument standard (`where("id", "=", 5)`).
   - Following this established framework convention, `QueryBuilder.join()` should support both 3-argument shorthand (`join("users", "posts.user_id", "users.id")`) and 4-argument standard (`join("users", "posts.user_id", "=", "users.id")`).
   - The same parameter normalization applies to `leftJoin` and `rightJoin`.
   - `innerJoin` should be provided as a fluent alias to `join`.
   - `crossJoin(table)` allows cartesian joins without an `ON` predicate.

2. **SQL Compilation Structure**:
   - In standard SQL syntax, `JOIN` clauses immediately follow the `FROM <table>` clause and precede the `WHERE` clause:
     `SELECT <cols> FROM <from_table> [JOIN <join_table> ON <first> <op> <second>...] [WHERE ...] [ORDER BY ...] [LIMIT ...]`
   - Each join item in `self.joins` can be recorded as:
     `map("type", "INNER", "table", table, "first", first_col, "op", op, "second", second_col)` (or modern Spid3r map literal `{ "type": "INNER", ... }`).
   - For `CROSS JOIN`, the clause appends `" CROSS JOIN " + j["table"]`.
   - For `INNER`, `LEFT`, `RIGHT`, the clause appends `" " + j["type"] + " JOIN " + j["table"] + " ON " + j["first"] + " " + j["op"] + " " + j["second"]`.

3. **Introspection Helpers (`toSql()` & `getBindings()`)**:
   - Making `build_select_sql()` public and adding `toSql() -> string` and `getBindings() -> list` allows unit testing the query builder without database roundtrips, and matches standard ORM interfaces (e.g. Laravel / Knex).

4. **Preserving Query Correctness in `count()`**:
   - If a query filters on columns from joined tables (e.g. `DB.table("posts").join("users", ...).where("users.status", "=", "active").count()`), omitting `self.joins` from `count()` causes SQLite to raise `no such column: users.status`.
   - Compiling `self.joins` into `count()` ensures join filters execute consistently.

5. **Concurrency Hardening via `PRAGMA busy_timeout = 5000;`**:
   - Multi-statement execution is supported in `db_handle.exec(...)`.
   - Extending line 287 to `"PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;"` ensures every opened connection across all application and test workflows has a 5-second lock recovery window, resolving transient Windows locking contention without adding runtime overhead.

---

## 3. Caveats

1. **SQLite Right Join Support**:
   - SQLite version 3.39.0+ supports `RIGHT JOIN` and `FULL OUTER JOIN`. Older bundled SQLite builds only support `LEFT OUTER JOIN` and `INNER JOIN`. Emitting `RIGHT JOIN` in the query builder is syntactically standard SQL, but unit tests executing against SQLite should focus primary execution assertions on `INNER JOIN` and `LEFT JOIN` (which are universal across all SQLite versions) while testing `RIGHT JOIN` and `CROSS JOIN` via SQL compilation.
2. **Read-Only Scope**:
   - As an explorer, this investigation makes NO direct changes to source files. Concrete, copy-pasteable code proposals are provided below for the implementer agent.

---

## 4. Conclusion & Proposed Implementation

### 4.1 Proposed Changes to `framework/Database/DB.matrix`

#### Diff 1: Join Methods & Introspection on `QueryBuilder`
Insert after line 79 (after `offset()`):

```matrix
    // ------------------------------------------------------------------------
    // Joins
    // ------------------------------------------------------------------------

    public fn join(self, table: string, first_col: string, op: string, second_col = null) {
        if second_col == null {
            second_col = op;
            op = "=";
        }
        self.joins.push(map(
            "type", "INNER",
            "table", table,
            "first", first_col,
            "op", op,
            "second", second_col
        ));
        return self;
    }

    public fn innerJoin(self, table: string, first_col: string, op: string, second_col = null) {
        return self.join(table, first_col, op, second_col);
    }

    public fn leftJoin(self, table: string, first_col: string, op: string, second_col = null) {
        if second_col == null {
            second_col = op;
            op = "=";
        }
        self.joins.push(map(
            "type", "LEFT",
            "table", table,
            "first", first_col,
            "op", op,
            "second", second_col
        ));
        return self;
    }

    public fn rightJoin(self, table: string, first_col: string, op: string, second_col = null) {
        if second_col == null {
            second_col = op;
            op = "=";
        }
        self.joins.push(map(
            "type", "RIGHT",
            "table", table,
            "first", first_col,
            "op", op,
            "second", second_col
        ));
        return self;
    }

    public fn crossJoin(self, table: string) {
        self.joins.push(map(
            "type", "CROSS",
            "table", table,
            "first", "",
            "op", "",
            "second", ""
        ));
        return self;
    }

    public fn toSql(self) -> string {
        let compiled = self.build_select_sql();
        return compiled["sql"];
    }

    public fn getBindings(self) {
        let compiled = self.build_select_sql();
        return compiled["params"];
    }
```

#### Diff 2: Join Compilation in `build_select_sql()` & `count()`
Replace `build_select_sql` (lines 85–114) and update `count` (lines 143–166):

```matrix
    public fn build_select_sql(self) {
        let sql = "SELECT " + self.select_cols.join(", ") + " FROM " + self.table_name;
        let params = list();

        if self.joins.len() > 0 {
            for j in self.joins {
                let j_type = j["type"];
                let j_table = j["table"];
                if j_type == "CROSS" {
                    sql = sql + " CROSS JOIN " + j_table;
                } else {
                    sql = sql + " " + j_type + " JOIN " + j_table + " ON " + j["first"] + " " + j["op"] + " " + j["second"];
                }
            }
        }

        if self.wheres.len() > 0 {
            sql = sql + " WHERE ";
            let first = true;
            for w in self.wheres {
                if not first {
                    sql = sql + " " + w["bool"] + " ";
                }
                sql = sql + w["col"] + " " + w["op"] + " ?";
                params.push(w["val"]);
                first = false;
            }
        }

        if self.order_col != "" {
            sql = sql + " ORDER BY " + self.order_col + " " + self.order_dir;
        }

        if self.limit_count > 0 {
            sql = sql + " LIMIT " + string(self.limit_count);
            if self.offset_count > 0 {
                sql = sql + " OFFSET " + string(self.offset_count);
            }
        }

        return map("sql", sql, "params", params);
    }
```

And in `count(self)`:
```matrix
    public fn count(self) -> int {
        let sql = "SELECT count(*) as c FROM " + self.table_name;
        let params = list();

        if self.joins.len() > 0 {
            for j in self.joins {
                let j_type = j["type"];
                let j_table = j["table"];
                if j_type == "CROSS" {
                    sql = sql + " CROSS JOIN " + j_table;
                } else {
                    sql = sql + " " + j_type + " JOIN " + j_table + " ON " + j["first"] + " " + j["op"] + " " + j["second"];
                }
            }
        }

        if self.wheres.len() > 0 {
            sql = sql + " WHERE ";
            let first = true;
            for w in self.wheres {
                if not first {
                    sql = sql + " " + w["bool"] + " ";
                }
                sql = sql + w["col"] + " " + w["op"] + " ?";
                params.push(w["val"]);
                first = false;
            }
        }

        let conn = self.get_connection();
        let rows = conn.query(sql, params);
        if rows.len() > 0 and rows[0].has("c") {
            return int(rows[0]["c"]);
        }
        return 0;
    }
```

#### Diff 3: Busy Timeout in `DatabaseManager.connection()`
In `framework/Database/DB.matrix` line 287:

```diff
-            self.db_handle.exec("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;");
+            self.db_handle.exec("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;");
```

---

### 4.2 Formulated Unit Tests for `tests/test_query_builder.matrix`

Add three dedicated test suites to `tests/test_query_builder.matrix`:

```matrix
fn test_query_builder_joins_sql() {
    // 1. Standard 4-arg inner join
    let q1 = DB.table("posts").join("users", "posts.user_id", "=", "users.id");
    assert(q1.toSql() == "SELECT * FROM posts INNER JOIN users ON posts.user_id = users.id", "q1 toSql mismatch");

    // 2. Shorthand 3-arg inner join
    let q2 = DB.table("posts").join("users", "posts.user_id", "users.id");
    assert(q2.toSql() == "SELECT * FROM posts INNER JOIN users ON posts.user_id = users.id", "q2 toSql mismatch");

    // 3. Explicit innerJoin alias
    let q3 = DB.table("posts").innerJoin("users", "posts.user_id", "=", "users.id");
    assert(q3.toSql() == "SELECT * FROM posts INNER JOIN users ON posts.user_id = users.id", "q3 toSql mismatch");

    // 4. Left join
    let q4 = DB.table("users").leftJoin("posts", "users.id", "=", "posts.user_id");
    assert(q4.toSql() == "SELECT * FROM users LEFT JOIN posts ON users.id = posts.user_id", "q4 toSql mismatch");

    // 5. Right join
    let q5 = DB.table("posts").rightJoin("users", "posts.user_id", "=", "users.id");
    assert(q5.toSql() == "SELECT * FROM posts RIGHT JOIN users ON posts.user_id = users.id", "q5 toSql mismatch");

    // 6. Cross join
    let q6 = DB.table("users").crossJoin("roles");
    assert(q6.toSql() == "SELECT * FROM users CROSS JOIN roles", "q6 toSql mismatch");

    // 7. Chained joins with select, where, orderBy, limit, offset
    let q7 = DB.table("posts")
        .select(list("posts.id", "posts.title", "users.name as author"))
        .join("users", "posts.user_id", "=", "users.id")
        .leftJoin("categories", "posts.category_id", "=", "categories.id")
        .where("posts.status", "=", "published")
        .where("users.id", ">", 0)
        .orderBy("posts.created_at", "DESC")
        .limit(10)
        .offset(5);

    let compiled7 = q7.build_select_sql();
    let expected_sql = "SELECT posts.id, posts.title, users.name as author FROM posts INNER JOIN users ON posts.user_id = users.id LEFT JOIN categories ON posts.category_id = categories.id WHERE posts.status = ? AND users.id > ? ORDER BY posts.created_at DESC LIMIT 10 OFFSET 5";
    assert(compiled7["sql"] == expected_sql, "q7 sql mismatch");
    assert(compiled7["params"].len() == 2, "q7 params count mismatch");
    assert(compiled7["params"][0] == "published", "q7 param 0 mismatch");
    assert(compiled7["params"][1] == 0, "q7 param 1 mismatch");
    assert(q7.toSql() == expected_sql, "q7 toSql mismatch");
    println("  [PASS] test_query_builder_joins_sql");
}

fn test_query_builder_joins_execution() {
    let now = string(unix_time());

    // 1. Create test user A with post
    let user_a_id = DB.table("users").insert(map(
        "name", "Join User A",
        "email", "join_a_" + now + "@example.com",
        "password", "secret",
        "created_at", now,
        "updated_at", now
    ));

    let post_id = DB.table("posts").insert(map(
        "title", "Join Test Post",
        "slug", "join-test-post-" + now,
        "content", "Join test content body",
        "category", "Tech",
        "user_id", user_a_id,
        "status", "published",
        "views_count", 15,
        "created_at", now,
        "updated_at", now
    ));

    // 2. Create test user B with no posts
    let user_b_id = DB.table("users").insert(map(
        "name", "Join User B",
        "email", "join_b_" + now + "@example.com",
        "password", "secret",
        "created_at", now,
        "updated_at", now
    ));

    // 3. Test INNER JOIN execution
    let inner_rows = DB.table("posts")
        .select(list("posts.title", "users.name", "posts.user_id"))
        .join("users", "posts.user_id", "=", "users.id")
        .where("posts.id", "=", post_id)
        .get();
    assert(inner_rows.len() == 1, "INNER JOIN should return 1 row");
    assert(inner_rows[0]["title"] == "Join Test Post", "Title should match");
    assert(inner_rows[0]["name"] == "Join User A", "Author name should match");

    // 4. Test LEFT JOIN execution (user with no posts)
    let left_rows = DB.table("users")
        .select(list("users.name", "posts.title"))
        .leftJoin("posts", "users.id", "=", "posts.user_id")
        .where("users.id", "=", user_b_id)
        .get();
    assert(left_rows.len() == 1, "LEFT JOIN should return 1 row for lonely user");
    assert(left_rows[0]["name"] == "Join User B", "User name should match");
    assert(left_rows[0]["title"] == null, "Post title should be null for user with no posts");

    // 5. Test count() with JOIN and WHERE condition
    let joined_count = DB.table("posts")
        .join("users", "posts.user_id", "=", "users.id")
        .where("users.id", "=", user_a_id)
        .count();
    assert(joined_count == 1, "Count with JOIN should return 1");

    // Teardown
    DB.table("posts").where("id", "=", post_id).delete();
    DB.table("users").where("id", "=", user_a_id).delete();
    DB.table("users").where("id", "=", user_b_id).delete();
    println("  [PASS] test_query_builder_joins_execution");
}

fn test_database_busy_timeout() {
    let conn = DB.connection();
    let rows = conn.query("PRAGMA busy_timeout;");
    assert(rows.len() > 0, "PRAGMA busy_timeout should return at least one row");
    assert(int(rows[0]["timeout"]) == 5000, "busy_timeout should be 5000 ms");
    println("  [PASS] test_database_busy_timeout");
}
```

Update `main()` in `tests/test_query_builder.matrix` to:
```matrix
fn main() {
    println("\n=== Running QueryBuilder Test Suite ===");
    test_query_builder_crud();
    test_query_builder_joins_sql();
    test_query_builder_joins_execution();
    test_database_busy_timeout();
    DB.close();
    println("=== All QueryBuilder Tests Passed! ===\n");
    return 0;
}
```

---

## 5. Verification Method

Once implemented by the milestone implementer, run the following verification steps:

1. **Verify QueryBuilder Unit Tests Pass**:
   ```powershell
   Set-Location "C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web"
   & "C:\Users\needyamin\OneDrive\Desktop\spid3r\target\debug\spid3r.exe" run tests/test_query_builder.matrix
   ```
   **Expected**:
   - `test_query_builder_crud` PASS
   - `test_query_builder_joins_sql` PASS
   - `test_query_builder_joins_execution` PASS
   - `test_database_busy_timeout` PASS (5000 ms timeout verified)
   - Exits code 0.

2. **Verify Full Framework Test Suite**:
   ```powershell
   Set-Location "C:\Users\needyamin\OneDrive\Desktop\spid3r\examples\spid3r_web"
   & "C:\Users\needyamin\OneDrive\Desktop\spid3r\target\debug\spid3r.exe" magic test
   ```
   **Expected**: 6/6 test suites passed, exits code 0.

3. **Verify Repository Cargo Tests Pass**:
   ```powershell
   Set-Location "C:\Users\needyamin\OneDrive\Desktop\spid3r"
   cargo test --test magic_tests
   cargo test --test web_stack_tests
   ```
   **Expected**: 0 failures across both suites.

4. **Invalidation Conditions**:
   - If calling `DB.table("posts").join(...)` throws `method 'join' not found on QueryBuilder`, joins are not exposed.
   - If `toSql()` generates SQL without `INNER JOIN ... ON ...`, join compilation is incomplete.
   - If `PRAGMA busy_timeout;` returns 0 instead of 5000, `DatabaseManager.connection()` was not updated.

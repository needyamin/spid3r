//! Compile the checked AST into bytecode for the Spid3r virtual machine.
//!
//! Design notes:
//! * The top-level program compiles to one "script" function. Its depth-0
//!   declarations are *globals* (`OpDefineGlobal`); declarations inside blocks
//!   or functions are locals.
//! * Locals and the operand stack share one contiguous per-frame slot vector
//!   (clox style). A local's slot is the index where its initializer sits on
//!   the stack at the point of declaration, so `add_local` records the current
//!   local count (params occupy slots 0..arity and are also tracked).
//! * A local captured by a nested function becomes an upvalue *cell* —
//!   a heap `UpvalueCell` shared by the defining frame and every closure over
//!   it. This keeps closures `Send` and correct across tasks.
//! * `try`/`catch`/`finally` compile to one `OpTry` plus an `OpEnterFinally` /
//!   `OpEndTry` pair; the VM owns unwinding via a handler stack.
//! * Class constructors (`new`) do not list `self` in the AST, but methods do.
//!   To keep slot numbering uniform, `new` is compiled with a synthetic `self`
//!   prepended, so every method's frame has `self` at slot 0.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::*;
use crate::diagnostics::{Diagnostic, Phase};
use crate::parser;
use crate::semantic;
use crate::value::collections::is_constructor;
use crate::value::Value;
use crate::vm::bytecode::*;

/// A variable resolved at compile time.
#[derive(Clone, Copy)]
enum Resolved {
    Local(u8),
    Upvalue(u8),
    Global,
}

struct Local {
    name: String,
    depth: usize,
    is_captured: bool,
    upvalue_index: Option<u8>,
}

struct LoopInfo {
    break_patches: Vec<usize>,
    continue_patches: Vec<usize>,
    /// `locals.len()` when the loop was entered. `break`/`continue` jump past
    /// the body scope's unwind Pops, so they must pop every local declared
    /// inside the body themselves (otherwise later locals get wrong slots).
    locals_base: usize,
}

struct Compiler {
    chunk: Chunk,
    locals: Vec<Local>,
    scope_depth: usize,
    is_global: bool,
    /// Enclosing-frame slot indices for this function's own upvalues, in order.
    upvalues: Vec<u8>,
    /// Count of this function's own locals that are captured.
    own_upvalues: u8,
    /// Public top-level names (only meaningful on the script compiler).
    public: HashSet<String>,
    loops: Vec<LoopInfo>,
    /// Locals newly marked captured since the last `Closure` was emitted:
    /// `(stack_slot, own_upvalue_index)` pairs whose value must be copied from
    /// the operand stack into their cell before the capturing closure exists.
    pending_syncs: Vec<(usize, u8)>,
    /// This program's import literals mapped to their resolved module paths.
    /// Only the script compiler's map is populated; `Stmt::Import` emits the
    /// resolved path so every spelling of a module shares one embedded entry.
    import_keys: HashMap<String, String>,
}

impl Compiler {
    fn new_script() -> Self {
        Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            scope_depth: 0,
            is_global: true,
            upvalues: Vec::new(),
            own_upvalues: 0,
            public: HashSet::new(),
            loops: Vec::new(),
            pending_syncs: Vec::new(),
            import_keys: HashMap::new(),
        }
    }

    fn new_function() -> Self {
        Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            scope_depth: 0,
            is_global: false,
            upvalues: Vec::new(),
            own_upvalues: 0,
            public: HashSet::new(),
            loops: Vec::new(),
            pending_syncs: Vec::new(),
            import_keys: HashMap::new(),
        }
    }

    fn begin_scope(&mut self) {
        self.scope_depth += 1;
    }

    /// Emit the Pops that unwind locals declared inside the innermost loop
    /// body. `break`/`continue` jump past the body scope's own unwind code, so
    /// without these the loop body's locals would leak onto the operand stack
    /// and every later local would be assigned the wrong slot.
    fn emit_loop_body_pops(&mut self) {
        if let Some(info) = self.loops.last() {
            let base = info.locals_base;
            let extra = self.locals.len().saturating_sub(base);
            // `break`/`continue` also skip the body scope's `end_scope`, so any
            // captured local dying here must release its cell routing too,
            // otherwise the raw slot would stay invisible until some later
            // declaration reuses the index and reads the stale cell.
            for i in (base..self.locals.len()).rev() {
                if self.locals[i].is_captured {
                    let slot = i as u8;
                    self.chunk.emit_u8(OpCode::Uncapture, slot);
                }
            }
            for _ in 0..extra {
                self.chunk.emit(OpCode::Pop);
            }
        }
    }

    fn end_scope(&mut self) {
        while let Some(l) = self.locals.last() {
            if l.depth == self.scope_depth {
                let was_captured = l.is_captured;
                let slot = (self.locals.len() - 1) as u8;
                self.locals.pop();
                if was_captured {
                    // The scope that owned the binding is gone: stop routing
                    // reads/writes of this slot through the shared cell, so a
                    // later local that reuses the slot index gets clean
                    // storage. The cell itself stays alive for the closures
                    // that captured it.
                    self.chunk.emit_u8(OpCode::Uncapture, slot);
                }
                self.chunk.emit(OpCode::Pop);
            } else {
                break;
            }
        }
        self.scope_depth -= 1;
    }

    /// Reserve a parameter slot. Params occupy slots 0..arity and are tracked
    /// alongside locals so `add_local` slot math stays correct.
    fn add_param(&mut self, name: &str) {
        self.locals.push(Local {
            name: name.to_string(),
            depth: 0,
            is_captured: false,
            upvalue_index: None,
        });
    }

    /// Declare a local; its initializer value is already on top of the stack.
    /// Returns the slot index assigned.
    fn add_local(&mut self, name: &str) -> u8 {
        let slot = self.locals.len() as u8;
        self.locals.push(Local {
            name: name.to_string(),
            depth: self.scope_depth,
            is_captured: false,
            upvalue_index: None,
        });
        slot
    }
}

/// Compile a whole program. Returns the script function and the set of public
/// top-level names (used when the program is imported as a module).
///
/// `importer_file` is the source path of the program being compiled; imports
/// resolve relative to its parent directory first (mirroring the interpreter and
/// VM). `module_dirs` are the additional search directories (SPID3R_PATH etc).
///
/// Every imported module — and all of its transitive imports — is resolved,
/// parsed, semantically checked, and recursively compiled, then embedded into
/// the returned script's `Chunk.modules`. This makes the produced `.matrixc`
/// self-contained: the VM's `exec` path can run it without any `.matrix` source
/// on disk.
pub fn compile_program(
    program: &Program,
    importer_file: &str,
    module_dirs: &[PathBuf],
) -> Result<(crate::value::VmFunc, HashSet<String>), Diagnostic> {
    let (mut func, public) = compile_program_unoptimized(program, importer_file, module_dirs)?;
    // Constant folding / peephole / dead-code removal (see vm::opt).
    super::opt::optimize_script(&mut func);
    Ok((func, public))
}

/// Compile without running the optimizer. Used by `--no-opt` and by the
/// optimizer's own tests (to compare optimised vs plain behaviour).
pub fn compile_program_unoptimized(
    program: &Program,
    importer_file: &str,
    module_dirs: &[PathBuf],
) -> Result<(crate::value::VmFunc, HashSet<String>), Diagnostic> {
    let mut embedded: Vec<EmbeddedModule> = Vec::new();
    let mut keys: HashSet<String> = HashSet::new();
    let mut in_progress: HashSet<String> = HashSet::new();
    in_progress.insert(importer_file.to_string());
    let (func, public) =
        compile_one(program, importer_file, module_dirs, &mut embedded, &mut keys, &mut in_progress, true)?;
    Ok((func, public))
}

/// Recursive core of `compile_program`. Compiles `program` to a script function
/// and, for each `import` it contains, resolves/compiles the target module and
/// appends it (plus all of its transitive imports) to the shared `embedded`
/// table so the final `.matrixc` is fully self-contained.
///
/// `Import` opcodes embed the module's RESOLVED path (canonicalized), not the
/// source literal: `import "lib/util"` and `import "./lib/util"` then share
/// one embedded entry and one runtime instance, and the runtime module cache
/// keys on the same string.
fn compile_one(
    program: &Program,
    importer_file: &str,
    module_dirs: &[PathBuf],
    embedded: &mut Vec<EmbeddedModule>,
    keys: &mut HashSet<String>,
    in_progress: &mut HashSet<String>,
    top: bool,
) -> Result<(crate::value::VmFunc, HashSet<String>), Diagnostic> {
    // Pre-resolve this program's imports: the `Import` ops will carry the
    // resolved path so every spelling of the same module shares one entry.
    let mut import_keys: HashMap<String, String> = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Import { path, span } = stmt {
            if import_keys.contains_key(path) {
                continue;
            }
            let resolved = resolve_module_path(importer_file, module_dirs, path).map_err(|mut e| {
                e.span = Some(span.clone());
                e
            })?;
            import_keys.insert(path.clone(), crate::modules::canonical_key(&resolved).to_string_lossy().to_string());
        }
    }
    let mut compilers: Vec<Compiler> = vec![Compiler::new_script()];
    compilers[0].import_keys = import_keys.clone();
    for stmt in &program.stmts {
        compile_stmt(&mut compilers, 0, stmt)?;
    }
    // The script returns null; the VM driver then calls `main`.
    compilers[0].chunk.emit(OpCode::Null);
    compilers[0].chunk.emit(OpCode::Return);
    let c = compilers.pop().unwrap();
    let public = c.public.clone();
    let own_upvalues = c.own_upvalues;
    let mut chunk = c.chunk;

    // Resolve and embed every import (directly, and transitively via recursion).
    for stmt in &program.stmts {
        if let Stmt::Import { path, .. } = stmt {
            let rkey = import_keys.get(path).cloned().unwrap_or_else(|| path.clone());
            if keys.contains(&rkey) {
                continue;
            }
            if in_progress.contains(&rkey) {
                return Err(Diagnostic::new(
                    Phase::Semantic,
                    format!("circular import of '{}'", path),
                ));
            }
            let src = std::fs::read_to_string(&rkey).map_err(|e| {
                Diagnostic::new(
                    Phase::Semantic,
                    format!("cannot import '{}': {}", path, e),
                )
            })?;
            let file: Arc<str> = Arc::from(rkey.clone());
            let mod_program = parser::parse_file(&src, file.clone())?;
            semantic::check_file(&mod_program, false, &rkey)?;
            in_progress.insert(rkey.clone());
            let (mod_func, mod_public) =
                compile_one(&mod_program, &file, module_dirs, embedded, keys, in_progress, false)?;
            in_progress.remove(&rkey);
            let public_vec: Vec<String> = mod_public.into_iter().collect();
            embedded.push(EmbeddedModule {
                key: rkey.clone(),
                public: public_vec,
                func: Arc::new(mod_func),
            });
            keys.insert(rkey);
        }
    }
    // Only the top-level script claims the embedded table. A nested (module)
    // compile used to `mem::take` it too, stealing every entry accumulated so
    // far into the module's own chunk: with two import spellings of the same
    // module the first embedded entry vanished from the `.matrixc` and `exec`
    // failed with "module not found". Module chunks never run through
    // `run_compiled`, so their `modules` field stays empty here; the top-level
    // table carries every entry.
    if top {
        chunk.modules = std::mem::take(embedded);
    }
    let func = crate::value::VmFunc {
        name: None,
        arity: 0,
        is_async: false,
        chunk: std::sync::Arc::new(chunk),
        upvalues: Vec::new(),
        own_upvalues,
    };
    Ok((func, public))
}

/// Resolve an import literal to a `.matrix` file on disk, mirroring the
/// interpreter's `resolve`: the importing file's parent directory first, then
/// each entry of `module_dirs` (with and without the `.matrix` suffix).
fn resolve_module_path(
    importer_file: &str,
    module_dirs: &[PathBuf],
    path: &str,
) -> Result<PathBuf, Diagnostic> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let base = PathBuf::from(importer_file);
    if let Some(dir) = base.parent() {
        candidates.push(dir.join(path));
        candidates.push(dir.join(format!("{}.matrix", path)));
    }
    for d in module_dirs {
        candidates.push(d.join(path));
        candidates.push(d.join(format!("{}.matrix", path)));
    }
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(Diagnostic::new(
        Phase::Semantic,
        format!(
            "module '{}' not found (looked next to the importing file and on SPID3R_PATH)",
            path
        ),
    ))
}

// ------------------------------------------------------------- statements

fn compile_stmt(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    stmt: &Stmt,
) -> Result<(), Diagnostic> {
    compilers[ci].chunk.set_span(stmt.span().clone());
    match stmt {
        Stmt::Expr(e) => {
            compile_expr(compilers, ci, e)?;
            compilers[ci].chunk.emit(OpCode::Pop);
        }
        Stmt::Let { name, value, vis, .. } => {
            if let Some(v) = value {
                compile_expr(compilers, ci, v)?;
            } else {
                compilers[ci].chunk.emit(OpCode::Null);
            }
            define_variable(compilers, ci, name, *vis);
        }
        Stmt::Const { name, value, vis, .. } => {
            compile_expr(compilers, ci, value)?;
            define_variable(compilers, ci, name, *vis);
        }
        Stmt::Fn(proto) => {
            let f = compile_function(compilers, ci, proto)?;
            let idx = compilers[ci].chunk.add_function(f);
            emit_pending_syncs(compilers, ci);
            compilers[ci].chunk.emit_u16(OpCode::Closure, idx);
            let vis = proto.vis;
            define_variable(compilers, ci, proto.name.as_deref().unwrap_or(""), vis);
        }
        Stmt::Class(decl) => {
            let c = compile_class(compilers, ci, decl)?;
            let idx = compilers[ci].chunk.add_class(c);
            compilers[ci].chunk.emit_u16(OpCode::Class, idx);
            define_variable(compilers, ci, &decl.name, decl.vis);
        }
        Stmt::Interface(decl) => {
            if compilers[ci].is_global
                && compilers[ci].scope_depth == 0
                && decl.vis == Vis::Public
            {
                compilers[0].public.insert(decl.name.clone());
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                compile_expr(compilers, ci, v)?;
            } else {
                compilers[ci].chunk.emit(OpCode::Null);
            }
            compilers[ci].chunk.emit(OpCode::Return);
        }
        Stmt::Break { .. } => {
            compilers[ci].emit_loop_body_pops();
            let pos = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
            compilers[ci]
                .loops
                .last_mut()
                .expect("break outside loop")
                .break_patches
                .push(pos);
        }
        Stmt::Continue { .. } => {
            compilers[ci].emit_loop_body_pops();
            let pos = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
            compilers[ci]
                .loops
                .last_mut()
                .expect("continue outside loop")
                .continue_patches
                .push(pos);
        }
        Stmt::Block(b) => compile_block(compilers, ci, &b.stmts)?,
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => compile_if(compilers, ci, cond, then, elifs, els)?,
        Stmt::While { cond, body, .. } => compile_while(compilers, ci, cond, body)?,
        Stmt::ForIn {
            var,
            iter,
            body,
            ..
        } => compile_for_in(compilers, ci, var, iter, body)?,
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            compile_stmt(compilers, ci, init)?;
            let start = compilers[ci].chunk.here();
            compile_expr(compilers, ci, cond)?;
            let exit_jump = emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfFalse);
            let locals_base = compilers[ci].locals.len();
            compilers[ci].loops.push(LoopInfo {
                break_patches: Vec::new(),
                continue_patches: Vec::new(),
                locals_base,
            });
            compile_block(compilers, ci, &body.stmts)?;
            let post_pos = compilers[ci].chunk.here();
            compile_expr(compilers, ci, post)?;
            compilers[ci].chunk.emit(OpCode::Pop);
            let loop_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
            let end = compilers[ci].chunk.here();
            patch_jump(&mut compilers[ci].chunk, exit_jump, end);
            patch_jump(&mut compilers[ci].chunk, loop_jump, start);
            let info = compilers[ci].loops.pop().unwrap();
            for p in info.break_patches {
                patch_jump(&mut compilers[ci].chunk, p, end);
            }
            for p in info.continue_patches {
                patch_jump(&mut compilers[ci].chunk, p, post_pos);
            }
        }
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => compile_try(compilers, ci, body, catch, finally)?,
        Stmt::Throw { value, .. } => {
            compile_expr(compilers, ci, value)?;
            compilers[ci].chunk.emit(OpCode::Throw);
        }
        Stmt::Spawn { callable, .. } => {
            // `Spawn` pushes the task handle; a bare `spawn ...;` statement
            // discards it, so pop it to keep the operand stack aligned with the
            // local-declaration slots (the clox invariant).
            compile_expr(compilers, ci, callable)?;
            compilers[ci].chunk.emit(OpCode::Spawn);
            compilers[ci].chunk.emit(OpCode::Pop);
        }
        Stmt::Import { path, .. } => {
            // Emit the resolved module path (the embedded table's key), so
            // two spellings of one module share a single entry and instance.
            let key = compilers[ci]
                .import_keys
                .get(path)
                .cloned()
                .unwrap_or_else(|| path.clone());
            let idx = compilers[ci].chunk.emit_str_const(&key);
            compilers[ci].chunk.emit_u16(OpCode::Import, idx);
        }
    }
    Ok(())
}

fn compile_block(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    stmts: &[Stmt],
) -> Result<(), Diagnostic> {
    compilers[ci].begin_scope();
    for s in stmts {
        compile_stmt(compilers, ci, s)?;
    }
    compilers[ci].end_scope();
    Ok(())
}

fn compile_if(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    cond: &Expr,
    then: &Block,
    elifs: &[(Expr, Block)],
    els: &Option<Block>,
) -> Result<(), Diagnostic> {
    compile_expr(compilers, ci, cond)?;
    let mut exit_jumps: Vec<usize> = Vec::new();
    let else_jump = emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfFalse);
    compile_block(compilers, ci, &then.stmts)?;
    exit_jumps.push(emit_jump(&mut compilers[ci].chunk, OpCode::Jump));
    let mut next = compilers[ci].chunk.here();
    patch_jump(&mut compilers[ci].chunk, else_jump, next);
    for (ec, eb) in elifs {
        compile_expr(compilers, ci, ec)?;
        let ej = emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfFalse);
        compile_block(compilers, ci, &eb.stmts)?;
        exit_jumps.push(emit_jump(&mut compilers[ci].chunk, OpCode::Jump));
        next = compilers[ci].chunk.here();
        patch_jump(&mut compilers[ci].chunk, ej, next);
    }
    if let Some(b) = els {
        compile_block(compilers, ci, &b.stmts)?;
    }
    let end = compilers[ci].chunk.here();
    for j in exit_jumps {
        patch_jump(&mut compilers[ci].chunk, j, end);
    }
    Ok(())
}

fn compile_while(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    cond: &Expr,
    body: &Block,
) -> Result<(), Diagnostic> {
    let start = compilers[ci].chunk.here();
    compile_expr(compilers, ci, cond)?;
    let exit_jump = emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfFalse);
    let locals_base = compilers[ci].locals.len();
    compilers[ci].loops.push(LoopInfo {
        break_patches: Vec::new(),
        continue_patches: Vec::new(),
        locals_base,
    });
    compile_block(compilers, ci, &body.stmts)?;
    let loop_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
    let end = compilers[ci].chunk.here();
    patch_jump(&mut compilers[ci].chunk, exit_jump, end);
    patch_jump(&mut compilers[ci].chunk, loop_jump, start);
    let info = compilers[ci].loops.pop().unwrap();
    for p in info.break_patches {
        patch_jump(&mut compilers[ci].chunk, p, end);
    }
    for p in info.continue_patches {
        patch_jump(&mut compilers[ci].chunk, p, start);
    }
    Ok(())
}

fn compile_for_in(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    var: &str,
    iter: &Expr,
    body: &Block,
) -> Result<(), Diagnostic> {
    // Materialize the iterable into a tuple on the stack, then index it.
    compile_expr(compilers, ci, iter)?;
    compilers[ci].chunk.emit(OpCode::ForEachInit);
    let items_slot = compilers[ci].add_local("__for_items");
    // push index 0
    let z = compilers[ci].chunk.emit_const(Value::Int(0));
    compilers[ci].chunk.emit_u16(OpCode::Constant, z);
    let idx_slot = compilers[ci].add_local("__for_idx");
    // reserved slot for the loop variable (bound each iteration)
    compilers[ci].chunk.emit(OpCode::Null);
    let var_slot = compilers[ci].add_local(var);

    let start = compilers[ci].chunk.here();
    // Release the previous iteration's cell for the loop variable (no-op on
    // the first pass), so this iteration's `CaptureSync` binds a fresh cell
    // and closures created here capture this iteration's value - matching the
    // tree interpreter, which defines a fresh binding per iteration.
    compilers[ci].chunk.emit_u8(OpCode::Uncapture, var_slot);
    // condition: idx < len(items)
    compilers[ci].chunk.emit_u8(OpCode::GetLocal, idx_slot);
    compilers[ci].chunk.emit_u8(OpCode::GetLocal, items_slot);
    compilers[ci].chunk.emit(OpCode::Len);
    compilers[ci].chunk.emit(OpCode::Less);
    let exit_jump = emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfFalse);

    // bind var = items[idx]
    compilers[ci].chunk.emit_u8(OpCode::GetLocal, items_slot);
    compilers[ci].chunk.emit_u8(OpCode::GetLocal, idx_slot);
    compilers[ci].chunk.emit(OpCode::IndexGet);
    compilers[ci].chunk.emit_u8(OpCode::SetLocal, var_slot);
    // `SetLocal` peeks (leaves the bound value on the stack); discard it since
    // this binding is a statement, not an expression that flows into another.
    compilers[ci].chunk.emit(OpCode::Pop);

    let locals_base = compilers[ci].locals.len();
    compilers[ci].loops.push(LoopInfo {
        break_patches: Vec::new(),
        continue_patches: Vec::new(),
        locals_base,
    });
    compile_block(compilers, ci, &body.stmts)?;
    let post = compilers[ci].chunk.here();
    // idx = idx + 1
    compilers[ci].chunk.emit_u8(OpCode::GetLocal, idx_slot);
    let one = compilers[ci].chunk.emit_const(Value::Int(1));
    compilers[ci].chunk.emit_u16(OpCode::Constant, one);
    compilers[ci].chunk.emit(OpCode::Add);
    compilers[ci].chunk.emit_u8(OpCode::SetLocal, idx_slot);
    compilers[ci].chunk.emit(OpCode::Pop);
    let loop_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
    let end = compilers[ci].chunk.here();
    patch_jump(&mut compilers[ci].chunk, exit_jump, end);
    patch_jump(&mut compilers[ci].chunk, loop_jump, start);
    let info = compilers[ci].loops.pop().unwrap();
    for p in info.break_patches {
        patch_jump(&mut compilers[ci].chunk, p, end);
    }
    for p in info.continue_patches {
        patch_jump(&mut compilers[ci].chunk, p, post);
    }
    // pop the three for-in locals from both the operand stack and the local
    // table; otherwise later declarations would be assigned the wrong slot.
    // A captured loop variable must also release its cell routing so the
    // recycled slot indices start clean.
    for slot in [items_slot, idx_slot, var_slot] {
        if compilers[ci].locals[slot as usize].is_captured {
            compilers[ci].chunk.emit_u8(OpCode::Uncapture, slot);
        }
    }
    for _ in 0..3 {
        compilers[ci].locals.pop();
        compilers[ci].chunk.emit(OpCode::Pop);
    }
    Ok(())
}

fn compile_try(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    body: &Block,
    catch: &Option<CatchClause>,
    finally: &Option<Block>,
) -> Result<(), Diagnostic> {
    let try_pos = compilers[ci].chunk.here();
    let has_catch = catch.is_some();
    compilers[ci].chunk.emit_try(has_catch, 0, 0, 0);
    let operand_end = try_pos + 7; // opcode + has_catch + slot + 2 + 2

    compilers[ci].begin_scope();
    compile_block(compilers, ci, &body.stmts)?;
    compilers[ci].end_scope();

    let success_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
    let catch_target = compilers[ci].chunk.here();
    let catch_slot_assigned: u8;
    if let Some(cc) = catch {
        compilers[ci].begin_scope();
        catch_slot_assigned = compilers[ci].add_local(&cc.name);
        compile_block(compilers, ci, &cc.body.stmts)?;
        compilers[ci].end_scope();
        let catch_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
        let finally_target = compilers[ci].chunk.here();
        // patch the catch slot back into OpTry
        compilers[ci].chunk.code[try_pos + 2] = catch_slot_assigned;
        compilers[ci].chunk.emit(OpCode::EnterFinally);
        if let Some(fb) = finally {
            compilers[ci].begin_scope();
            compile_block(compilers, ci, &fb.stmts)?;
            compilers[ci].end_scope();
        }
        compilers[ci].chunk.emit(OpCode::EndTry);
        let _ = finally_target;
        let _end = compilers[ci].chunk.here();
        compilers[ci]
            .chunk
            .patch_i16(try_pos + 3, (catch_target as i64 - operand_end as i64) as i16);
        compilers[ci]
            .chunk
            .patch_i16(try_pos + 5, (finally_target as i64 - operand_end as i64) as i16);
        patch_jump(&mut compilers[ci].chunk, success_jump, finally_target);
        patch_jump(&mut compilers[ci].chunk, catch_jump, finally_target);
    } else {
        let finally_target = compilers[ci].chunk.here();
        compilers[ci].chunk.emit(OpCode::EnterFinally);
        if let Some(fb) = finally {
            compilers[ci].begin_scope();
            compile_block(compilers, ci, &fb.stmts)?;
            compilers[ci].end_scope();
        }
        compilers[ci].chunk.emit(OpCode::EndTry);
        let _end = compilers[ci].chunk.here();
        compilers[ci]
            .chunk
            .patch_i16(try_pos + 3, (finally_target as i64 - operand_end as i64) as i16);
        compilers[ci]
            .chunk
            .patch_i16(try_pos + 5, (finally_target as i64 - operand_end as i64) as i16);
        patch_jump(&mut compilers[ci].chunk, success_jump, finally_target);
    }
    Ok(())
}

// ------------------------------------------------------------- functions

fn compile_function(
    compilers: &mut Vec<Compiler>,
    _ci: usize,
    proto: &FnProto,
) -> Result<VmFuncProto, Diagnostic> {
    compilers.push(Compiler::new_function());
    let sub = compilers.len() - 1;
    for p in &proto.params {
        compilers[sub].add_param(&p.name);
    }
    compilers[sub].begin_scope();
    for s in &proto.body.stmts {
        compile_stmt(compilers, sub, s)?;
    }
    compilers[sub].end_scope();
    // Fall-through return null.
    compilers[sub].chunk.emit(OpCode::Null);
    compilers[sub].chunk.emit(OpCode::Return);
    let c = compilers.pop().unwrap();
    Ok(VmFuncProto {
        name: proto.name.clone(),
        arity: proto.params.len(),
        is_async: proto.is_async,
        upvalues: c.upvalues,
        own_upvalues: c.own_upvalues,
        chunk: std::sync::Arc::new(c.chunk),
    })
}

fn compile_class(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    decl: &ClassDecl,
) -> Result<VmClassProto, Diagnostic> {
    let mut methods = Vec::new();
    for m in &decl.methods {
        // Constructors do not list `self`; prepend a synthetic one so the
        // ctor frame matches method frames (self at slot 0).
        let proto: FnProto = if m.name.as_deref() == Some("new") {
            let mut p = m.clone();
            p.params.insert(
                0,
                Param {
                    name: "self".to_string(),
                    ty: None,
                    span: m.span.clone(),
                },
            );
            p
        } else {
            m.clone()
        };
        let f = compile_function(compilers, ci, &proto)?;
        let idx = compilers[ci].chunk.add_function(f);
        methods.push((
            m.name.clone().unwrap_or_default(),
            m.vis,
            idx,
            m.name.as_deref() == Some("new"),
        ));
    }
    Ok(VmClassProto {
        name: decl.name.clone(),
        parent: decl.parent.clone(),
        fields: decl.fields.iter().map(|f| (f.name.clone(), f.vis)).collect(),
        methods,
    })
}

// ------------------------------------------------------------- expressions

fn compile_expr(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    e: &Expr,
) -> Result<(), Diagnostic> {
    compilers[ci].chunk.set_span(e.span().clone());
    match e {
        Expr::Int(i, _) => {
            let idx = compilers[ci].chunk.emit_const(Value::Int(*i));
            compilers[ci].chunk.emit_u16(OpCode::Constant, idx);
        }
        Expr::Float(f, _) => {
            let idx = compilers[ci].chunk.emit_const(Value::Float(*f));
            compilers[ci].chunk.emit_u16(OpCode::Constant, idx);
        }
        Expr::Str(s, _) => {
            let idx = compilers[ci].chunk.emit_const(Value::Str(s.clone().into()));
            compilers[ci].chunk.emit_u16(OpCode::Constant, idx);
        }
        Expr::Bool(b, _) => {
            compilers[ci]
                .chunk
                .emit(if *b { OpCode::True } else { OpCode::False });
        }
        Expr::Null(_) => compilers[ci].chunk.emit(OpCode::Null),
        Expr::Ident(name, _) => {
            if name == "none" {
                compilers[ci].chunk.emit(OpCode::None);
            } else {
                emit_get(compilers, ci, name);
            }
        }
        Expr::List(items, _) => {
            for it in items {
                compile_expr(compilers, ci, it)?;
            }
            let idx = compilers[ci].chunk.emit_str_const("list");
            compilers[ci]
                .chunk
                .emit_u16_u8(OpCode::Construct, idx, items.len() as u8);
        }
        Expr::Fn(proto) => {
            let f = compile_function(compilers, ci, proto)?;
            let idx = compilers[ci].chunk.add_function(f);
            emit_pending_syncs(compilers, ci);
            compilers[ci].chunk.emit_u16(OpCode::Closure, idx);
        }
        Expr::Unary { op, expr, .. } => {
            compile_expr(compilers, ci, expr)?;
            match op {
                UnOp::Neg => compilers[ci].chunk.emit(OpCode::Neg),
                UnOp::Not => compilers[ci].chunk.emit(OpCode::Not),
            }
        }
        Expr::Binary { op, l, r, .. } => {
            compile_expr(compilers, ci, l)?;
            compile_expr(compilers, ci, r)?;
            let oc = match op {
                BinOp::Add => OpCode::Add,
                BinOp::Sub => OpCode::Sub,
                BinOp::Mul => OpCode::Mul,
                BinOp::Div => OpCode::Div,
                BinOp::Mod => OpCode::Mod,
                BinOp::Eq => OpCode::Equal,
                BinOp::Ne => OpCode::NotEqual,
                BinOp::Lt => OpCode::Less,
                BinOp::Le => OpCode::LessEq,
                BinOp::Gt => OpCode::Greater,
                BinOp::Ge => OpCode::GreaterEq,
            };
            compilers[ci].chunk.emit(oc);
        }
        Expr::Logical { op, l, r, .. } => {
            compile_expr(compilers, ci, l)?;
            match op {
                LogOp::And => {
                    let false_jump =
                        emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfFalse);
                    compile_expr(compilers, ci, r)?;
                    compilers[ci].chunk.emit(OpCode::Truthy);
                    let end_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
                    let f = compilers[ci].chunk.here();
                    patch_jump(&mut compilers[ci].chunk, false_jump, f);
                    compilers[ci].chunk.emit(OpCode::False);
                    let end = compilers[ci].chunk.here();
                    patch_jump(&mut compilers[ci].chunk, end_jump, end);
                }
                LogOp::Or => {
                    let true_jump =
                        emit_jump(&mut compilers[ci].chunk, OpCode::JumpIfTrue);
                    compile_expr(compilers, ci, r)?;
                    compilers[ci].chunk.emit(OpCode::Truthy);
                    let end_jump = emit_jump(&mut compilers[ci].chunk, OpCode::Jump);
                    let t = compilers[ci].chunk.here();
                    patch_jump(&mut compilers[ci].chunk, true_jump, t);
                    compilers[ci].chunk.emit(OpCode::True);
                    let end = compilers[ci].chunk.here();
                    patch_jump(&mut compilers[ci].chunk, end_jump, end);
                }
            }
        }
        Expr::Assign {
            target,
            op,
            value,
            ..
        } => compile_assign(compilers, ci, target, *op, value)?,
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name, _) = &**callee {
                if is_constructor(name) {
                    for a in args {
                        compile_expr(compilers, ci, a)?;
                    }
                    let idx = compilers[ci].chunk.emit_str_const(name);
                    compilers[ci]
                        .chunk
                        .emit_u16_u8(OpCode::Construct, idx, args.len() as u8);
                    return Ok(());
                }
            }
            compile_expr(compilers, ci, callee)?;
            for a in args {
                compile_expr(compilers, ci, a)?;
            }
            compilers[ci].chunk.emit_u8(OpCode::Call, args.len() as u8);
        }
        Expr::Method { object, name, args, .. } => {
            compile_expr(compilers, ci, object)?;
            for a in args {
                compile_expr(compilers, ci, a)?;
            }
            let idx = compilers[ci].chunk.emit_str_const(name);
            compilers[ci]
                .chunk
                .emit_u16_u8(OpCode::Invoke, idx, args.len() as u8);
        }
        Expr::Index { object, index, .. } => {
            compile_expr(compilers, ci, object)?;
            compile_expr(compilers, ci, index)?;
            compilers[ci].chunk.emit(OpCode::IndexGet);
        }
        Expr::Field { object, name, .. } => {
            compile_expr(compilers, ci, object)?;
            let idx = compilers[ci].chunk.emit_str_const(name);
            compilers[ci].chunk.emit_u16(OpCode::FieldGet, idx);
        }
        Expr::New { class, args, .. } => {
            // `Call` pops the argument(s) first, then the callee, so the class
            // (the callee) must be pushed *before* its arguments.
            emit_get(compilers, ci, class);
            for a in args {
                compile_expr(compilers, ci, a)?;
            }
            compilers[ci].chunk.emit_u8(OpCode::Call, args.len() as u8);
        }
        Expr::Await(inner, _) => {
            compile_expr(compilers, ci, inner)?;
            compilers[ci].chunk.emit(OpCode::Await);
        }
        Expr::Spawn { callable, .. } => {
            compile_expr(compilers, ci, callable)?;
            compilers[ci].chunk.emit(OpCode::Spawn);
        }
    }
    Ok(())
}

fn compile_assign(
    compilers: &mut Vec<Compiler>,
    ci: usize,
    target: &Expr,
    op: AssignOp,
    value: &Expr,
) -> Result<(), Diagnostic> {
    match target {
        Expr::Ident(name, _) => {
            if op != AssignOp::Assign {
                emit_get(compilers, ci, name);
                compile_expr(compilers, ci, value)?;
                emit_binop(compilers, ci, op);
            } else {
                compile_expr(compilers, ci, value)?;
            }
            emit_set(compilers, ci, name);
        }
        Expr::Index { object, index, .. } => {
            compile_expr(compilers, ci, object)?;
            compile_expr(compilers, ci, index)?;
            if op != AssignOp::Assign {
                // Duplicate `obj idx` before IndexGet consumes them, so
                // IndexSet still has the target below the new value.
                compilers[ci].chunk.emit(OpCode::Dup2);
                compilers[ci].chunk.emit(OpCode::IndexGet);
                compile_expr(compilers, ci, value)?;
                emit_binop(compilers, ci, op);
            } else {
                compile_expr(compilers, ci, value)?;
            }
            compilers[ci].chunk.emit(OpCode::IndexSet);
        }
        Expr::Field { object, name, .. } => {
            compile_expr(compilers, ci, object)?;
            if op != AssignOp::Assign {
                // Duplicate the object before FieldGet consumes it, so
                // FieldSet still has the target below the new value.
                compilers[ci].chunk.emit(OpCode::Dup);
                let idx = compilers[ci].chunk.emit_str_const(name);
                compilers[ci].chunk.emit_u16(OpCode::FieldGet, idx);
                compile_expr(compilers, ci, value)?;
                emit_binop(compilers, ci, op);
            } else {
                compile_expr(compilers, ci, value)?;
            }
            let idx = compilers[ci].chunk.emit_str_const(name);
            compilers[ci].chunk.emit_u16(OpCode::FieldSet, idx);
        }
        _ => {
            // Invalid target; the semantic pass rejects this. Emit nothing.
        }
    }
    Ok(())
}

fn emit_binop(compilers: &mut [Compiler], ci: usize, op: AssignOp) {
    let oc = match op {
        AssignOp::AddAssign => OpCode::Add,
        AssignOp::SubAssign => OpCode::Sub,
        AssignOp::MulAssign => OpCode::Mul,
        AssignOp::DivAssign => OpCode::Div,
        AssignOp::ModAssign => OpCode::Mod,
        AssignOp::Assign => return,
    };
    compilers[ci].chunk.emit(oc);
}

// ------------------------------------------------------- name resolution

fn find_local_full(c: &Compiler, name: &str) -> Option<(u8, bool, Option<u8>)> {
    let n = c.locals.len();
    for i in (0..n).rev() {
        if c.locals[i].name == name {
            return Some((i as u8, c.locals[i].is_captured, c.locals[i].upvalue_index));
        }
    }
    None
}

fn find_local_slot(c: &Compiler, name: &str) -> Option<usize> {
    c.locals.iter().rposition(|l| l.name == name)
}

fn capture_local(c: &mut Compiler, slot: usize) -> u8 {
    if let Some(idx) = c.locals[slot].upvalue_index {
        return idx;
    }
    let idx = c.own_upvalues;
    c.own_upvalues += 1;
    c.locals[slot].is_captured = true;
    c.locals[slot].upvalue_index = Some(idx);
    // The local's current value lives on the operand stack at `slot`; it must be
    // copied into the newly created cell before the capturing closure runs.
    c.pending_syncs.push((slot, idx));
    idx
}

/// Resolve a name in the enclosing scope of `ci`, returning the upvalue-slot
/// index in `ci` to capture (or `None` if it is a global). Captures the local
/// in the enclosing frame so the cell is shared.
fn resolve_upvalue(compilers: &mut Vec<Compiler>, ci: usize, name: &str) -> Option<u8> {
    if ci == 0 {
        return None;
    }
    let parent = ci - 1;
    let slot = find_local_slot(&compilers[parent], name);
    if let Some(slot) = slot {
        let up = capture_local(&mut compilers[parent], slot);
        // The runtime `GetUpvalue` index space is the parent function's full
        // upvalue list: its own captured-local cells come first, then the
        // cells it captured from *its* enclosing frame. Without the
        // `own_upvalues` offset, a function that both captures an enclosing
        // variable and has a local captured by a nested function would give
        // both the same index and silently alias two different variables.
        let my = compilers[ci].own_upvalues + compilers[ci].upvalues.len() as u8;
        compilers[ci].upvalues.push(up);
        return Some(my);
    }
    if let Some(grand) = resolve_upvalue(compilers, parent, name) {
        let my = compilers[ci].own_upvalues + compilers[ci].upvalues.len() as u8;
        compilers[ci].upvalues.push(grand);
        return Some(my);
    }
    None
}

fn resolve_variable(compilers: &mut Vec<Compiler>, ci: usize, name: &str) -> Resolved {
    if let Some((slot, captured, upidx)) = find_local_full(&compilers[ci], name) {
        if captured {
            return Resolved::Upvalue(upidx.unwrap());
        }
        return Resolved::Local(slot);
    }
    if let Some(up) = resolve_upvalue(compilers, ci, name) {
        return Resolved::Upvalue(up);
    }
    Resolved::Global
}

fn emit_get(compilers: &mut Vec<Compiler>, ci: usize, name: &str) {
    match resolve_variable(compilers, ci, name) {
        Resolved::Local(slot) => compilers[ci].chunk.emit_u8(OpCode::GetLocal, slot),
        Resolved::Upvalue(up) => compilers[ci].chunk.emit_u8(OpCode::GetUpvalue, up),
        Resolved::Global => {
            let idx = compilers[ci].chunk.emit_str_const(name);
            compilers[ci].chunk.emit_u16(OpCode::GetGlobal, idx);
        }
    }
}

fn emit_set(compilers: &mut Vec<Compiler>, ci: usize, name: &str) {
    match resolve_variable(compilers, ci, name) {
        Resolved::Local(slot) => compilers[ci].chunk.emit_u8(OpCode::SetLocal, slot),
        Resolved::Upvalue(up) => compilers[ci].chunk.emit_u8(OpCode::SetUpvalue, up),
        Resolved::Global => {
            let idx = compilers[ci].chunk.emit_str_const(name);
            compilers[ci].chunk.emit_u16(OpCode::SetGlobal, idx);
        }
    }
}

fn define_variable(compilers: &mut [Compiler], ci: usize, name: &str, vis: Vis) {
    if compilers[ci].is_global && compilers[ci].scope_depth == 0 {
        let idx = compilers[ci].chunk.emit_str_const(name);
        compilers[ci].chunk.emit_u16(OpCode::DefineGlobal, idx);
        if vis == Vis::Public {
            compilers[0].public.insert(name.to_string());
        }
    } else {
        compilers[ci].add_local(name);
    }
}

/// Emit code that binds each newly captured local to its own-upvalue cell
/// (`CaptureSync slot idx`). Must run just before a `Closure` opcode: the
/// binding copies the local's current stack value into the cell (so the
/// capturing closure sees it) and registers the slot in the frame's runtime
/// `captured` map, so all later `GetLocal`/`SetLocal` of that slot go through
/// the same cell the closure shares. Without the map, pre-capture emissions
/// (a loop condition, say) read the stale slot forever while the body writes
/// the cell.
fn emit_pending_syncs(compilers: &mut [Compiler], ci: usize) {
    let syncs = std::mem::take(&mut compilers[ci].pending_syncs);
    for (slot, idx) in syncs {
        compilers[ci].chunk.emit_u8_u8(OpCode::CaptureSync, slot as u8, idx);
    }
}

// --------------------------------------------------------------- jump util

fn emit_jump(chunk: &mut Chunk, op: OpCode) -> usize {
    chunk.emit(op);
    chunk.code.push(0);
    chunk.code.push(0);
    chunk.code.len() - 2
}

fn patch_jump(chunk: &mut Chunk, operand_pos: usize, target: usize) {
    let off = (target as i64 - (operand_pos as i64 + 2)) as i16;
    chunk.patch_i16(operand_pos, off);
}

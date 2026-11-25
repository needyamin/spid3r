//! LLVM target-initialization shims for the `native` feature.
//!
//! In LLVM >= 18 the `LLVM_Initialize*All` aggregates are `static inline`
//! functions in `llvm-c/Target.h`: they are never *exported* by the library,
//! which only exports the per-target initializers. inkwell's bindings still
//! declare the aggregates as extern symbols, so on a dynamically linked
//! Windows LLVM (`LLVM-C.dll`) they would be unresolved. We provide the
//! out-of-line definitions here — exactly what the header itself would have
//! emitted in a C consumer — for every target the official LLVM 18 releases
//! support. inkwell's `Target::initialize_all` / `initialize_native` then work
//! unchanged on every platform.

use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::OptimizationLevel;

#[cfg(windows)]
mod win_shims {
    unsafe extern "C" {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn LLVMInitializeX86TargetInfo();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn LLVMInitializeX86Target();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn LLVMInitializeX86TargetMC();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn LLVMInitializeX86AsmPrinter();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn LLVMInitializeX86AsmParser();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        fn LLVMInitializeX86Disassembler();

        #[cfg(target_arch = "aarch64")]
        fn LLVMInitializeAArch64TargetInfo();
        #[cfg(target_arch = "aarch64")]
        fn LLVMInitializeAArch64Target();
        #[cfg(target_arch = "aarch64")]
        fn LLVMInitializeAArch64TargetMC();
        #[cfg(target_arch = "aarch64")]
        fn LLVMInitializeAArch64AsmPrinter();
        #[cfg(target_arch = "aarch64")]
        fn LLVMInitializeAArch64AsmParser();
        #[cfg(target_arch = "aarch64")]
        fn LLVMInitializeAArch64Disassembler();

        #[cfg(target_arch = "arm")]
        fn LLVMInitializeARMTargetInfo();
        #[cfg(target_arch = "arm")]
        fn LLVMInitializeARMTarget();
        #[cfg(target_arch = "arm")]
        fn LLVMInitializeARMTargetMC();
        #[cfg(target_arch = "arm")]
        fn LLVMInitializeARMAsmPrinter();
        #[cfg(target_arch = "arm")]
        fn LLVMInitializeARMAsmParser();
        #[cfg(target_arch = "arm")]
        fn LLVMInitializeARMDisassembler();

        #[cfg(target_arch = "riscv64")]
        fn LLVMInitializeRISCVTargetInfo();
        #[cfg(target_arch = "riscv64")]
        fn LLVMInitializeRISCVTarget();
        #[cfg(target_arch = "riscv64")]
        fn LLVMInitializeRISCVTargetMC();
        #[cfg(target_arch = "riscv64")]
        fn LLVMInitializeRISCVAsmPrinter();
        #[cfg(target_arch = "riscv64")]
        fn LLVMInitializeRISCVAsmParser();
        #[cfg(target_arch = "riscv64")]
        fn LLVMInitializeRISCVDisassembler();

        #[cfg(target_arch = "wasm32")]
        fn LLVMInitializeWebAssemblyTargetInfo();
        #[cfg(target_arch = "wasm32")]
        fn LLVMInitializeWebAssemblyTarget();
        #[cfg(target_arch = "wasm32")]
        fn LLVMInitializeWebAssemblyTargetMC();
        #[cfg(target_arch = "wasm32")]
        fn LLVMInitializeWebAssemblyAsmPrinter();
        #[cfg(target_arch = "wasm32")]
        fn LLVMInitializeWebAssemblyAsmParser();
        #[cfg(target_arch = "wasm32")]
        fn LLVMInitializeWebAssemblyDisassembler();
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeAllTargetInfos() {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86TargetInfo();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64TargetInfo();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMTargetInfo();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVTargetInfo();
            #[cfg(target_arch = "wasm32")]
            LLVMInitializeWebAssemblyTargetInfo();
        }
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeAllTargets() {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86Target();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64Target();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMTarget();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVTarget();
            #[cfg(target_arch = "wasm32")]
            LLVMInitializeWebAssemblyTarget();
        }
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeAllTargetMCs() {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86TargetMC();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64TargetMC();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMTargetMC();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVTargetMC();
            #[cfg(target_arch = "wasm32")]
            LLVMInitializeWebAssemblyTargetMC();
        }
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeAllAsmPrinters() {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86AsmPrinter();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64AsmPrinter();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMAsmPrinter();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVAsmPrinter();
            #[cfg(target_arch = "wasm32")]
            LLVMInitializeWebAssemblyAsmPrinter();
        }
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeAllAsmParsers() {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86AsmParser();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64AsmParser();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMAsmParser();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVAsmParser();
            #[cfg(target_arch = "wasm32")]
            LLVMInitializeWebAssemblyAsmParser();
        }
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeAllDisassemblers() {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86Disassembler();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64Disassembler();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMDisassembler();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVDisassembler();
            #[cfg(target_arch = "wasm32")]
            LLVMInitializeWebAssemblyDisassembler();
        }
    }

    /// `LLVMBool` is `i32` (0 = success).
    #[no_mangle]
    pub extern "C" fn LLVM_InitializeNativeTarget() -> i32 {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                LLVMInitializeX86TargetInfo();
                LLVMInitializeX86Target();
                LLVMInitializeX86TargetMC();
            }
            #[cfg(target_arch = "aarch64")]
            {
                LLVMInitializeAArch64TargetInfo();
                LLVMInitializeAArch64Target();
                LLVMInitializeAArch64TargetMC();
            }
            #[cfg(target_arch = "arm")]
            {
                LLVMInitializeARMTargetInfo();
                LLVMInitializeARMTarget();
                LLVMInitializeARMTargetMC();
            }
            #[cfg(target_arch = "riscv64")]
            {
                LLVMInitializeRISCVTargetInfo();
                LLVMInitializeRISCVTarget();
                LLVMInitializeRISCVTargetMC();
            }
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeNativeAsmPrinter() -> i32 {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86AsmPrinter();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64AsmPrinter();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMAsmPrinter();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVAsmPrinter();
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeNativeAsmParser() -> i32 {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86AsmParser();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64AsmParser();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMAsmParser();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVAsmParser();
        }
        0
    }

    #[no_mangle]
    pub extern "C" fn LLVM_InitializeNativeDisassembler() -> i32 {
        // SAFETY: LLVM target registration C-API call is guarded by std::sync::Once and takes no borrowed pointers.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            LLVMInitializeX86Disassembler();
            #[cfg(target_arch = "aarch64")]
            LLVMInitializeAArch64Disassembler();
            #[cfg(target_arch = "arm")]
            LLVMInitializeARMDisassembler();
            #[cfg(target_arch = "riscv64")]
            LLVMInitializeRISCVDisassembler();
        }
        0
    }
}
/// CPU string for a target triple, used as the LLVM target-machine CPU.
pub fn cpu_for_triple(triple: &str) -> String {
    let arch = triple.split('-').next().unwrap_or("");
    match arch {
        "x86_64" => "x86-64".to_string(),
        "aarch64" => "generic".to_string(),
        "arm64" => "apple-a14".to_string(),
        "armv7" | "arm" => "generic".to_string(),
        "wasm32" | "wasm64" => "generic".to_string(),
        "riscv64" => "generic-rv64".to_string(),
        _ => "generic".to_string(),
    }
}

/// Emit a module to a native object file for the given triple.
pub fn emit_object(
    module: &inkwell::module::Module,
    triple: &str,
    opt: bool,
    out: &std::path::Path,
) -> Result<(), String> {
    emit_object_with_cpu(module, triple, None, opt, out)
}

/// Emit a module to a native object file for the given triple and optional
/// target CPU (`x86-64`, `x86-64-v2`, `x86-64-v3`, or `native`).
pub fn emit_object_with_cpu(
    module: &inkwell::module::Module,
    triple: &str,
    target_cpu: Option<&str>,
    opt: bool,
    out: &std::path::Path,
) -> Result<(), String> {
    // Initialize the two AOT target architectures directly (matching the
    // `target-x86` + `target-aarch64` features in Cargo.toml) instead of
    // `Target::initialize_all`, which calls `llvm-sys`'s `LLVM_InitializeAll*`
    // C wrappers that fail to link on `x86_64-apple-darwin` under
    // `llvm23-1-no-llvm-linking`.
    let init_cfg = InitializationConfig {
        asm_parser: false,
        asm_printer: true,
        base: true,
        disassembler: false,
        info: true,
        machine_code: true,
    };
    Target::initialize_x86(&init_cfg);
    Target::initialize_aarch64(&init_cfg);
    let triple_val = inkwell::targets::TargetTriple::create(triple);
    let target = Target::from_triple(&triple_val)
        .map_err(|e| format!("unknown target '{triple}': {e:?}"))?;
    let level = if opt {
        OptimizationLevel::Aggressive
    } else {
        OptimizationLevel::None
    };
    let (cpu, features): (String, String) = match target_cpu {
        Some("native") => (
            TargetMachine::get_host_cpu_name().to_string(),
            TargetMachine::get_host_cpu_features().to_string(),
        ),
        Some(c) => (c.to_string(), String::new()),
        None => {
            #[allow(unused_mut)]
            let mut default_cpu = cpu_for_triple(triple);
            if opt
                && std::env::var_os("SPID3R_PORTABLE_CPU").is_none()
                && triple == crate::native::link::default_triple()
            {
                #[cfg(target_arch = "x86_64")]
                if std::arch::is_x86_feature_detected!("avx2")
                    && std::arch::is_x86_feature_detected!("bmi2")
                    && std::arch::is_x86_feature_detected!("fma")
                {
                    default_cpu = "x86-64-v3".to_string();
                }
            }
            (default_cpu, String::new())
        }
    };
    let tm: TargetMachine = target
        .create_target_machine(
            &triple_val,
            &cpu,
            &features,
            level,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| format!("cannot create a target machine for '{triple}'"))?;
    if opt {
        let opts = inkwell::passes::PassBuilderOptions::create();
        opts.set_loop_vectorization(true);
        opts.set_loop_interleaving(true);
        opts.set_loop_slp_vectorization(true);
        opts.set_loop_unrolling(true);
        opts.set_merge_functions(true);
        let _ = module.run_passes("default<O3>", &tm, opts);
    }
    tm.write_to_file(module, FileType::Object, out)
        .map_err(|e| format!("cannot emit '{triple}' object: {e:?}"))
}

#[cfg(test)]
mod tests {
    use inkwell::attributes::Attribute;

    #[test]
    fn llvm23_attribute_kind_ids_are_nonzero() {
        for name in ["nounwind", "willreturn", "alwaysinline", "memory"] {
            let id = Attribute::get_named_enum_kind_id(name);
            assert_ne!(
                id, 0,
                "LLVM enum attribute kind id for '{name}' must be non-zero"
            );
        }
    }
}

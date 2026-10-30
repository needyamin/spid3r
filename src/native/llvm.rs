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
    Target::initialize_all(&InitializationConfig::default());
    let triple_val = inkwell::targets::TargetTriple::create(triple);
    let target = Target::from_triple(&triple_val)
        .map_err(|e| format!("unknown target '{triple}': {e:?}"))?;
    let level = if opt {
        OptimizationLevel::Aggressive
    } else {
        OptimizationLevel::None
    };
    let tm: TargetMachine = target
        .create_target_machine(
            &triple_val,
            &cpu_for_triple(triple),
            "",
            level,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| format!("cannot create a target machine for '{triple}'"))?;
    if opt {
        let opts = inkwell::passes::PassBuilderOptions::create();
        let _ = module.run_passes("default<O3>", &tm, opts);
    }
    tm.write_to_file(module, FileType::Object, out)
        .map_err(|e| format!("cannot emit '{triple}' object: {e:?}"))
}

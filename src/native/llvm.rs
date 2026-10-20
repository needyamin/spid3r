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

unsafe extern "C" {
    fn LLVMInitializeAArch64TargetInfo();
    fn LLVMInitializeAMDGPUTargetInfo();
    fn LLVMInitializeARMTargetInfo();
    fn LLVMInitializeAVRTargetInfo();
    fn LLVMInitializeBPFTargetInfo();
    fn LLVMInitializeHexagonTargetInfo();
    fn LLVMInitializeLanaiTargetInfo();
    fn LLVMInitializeLoongArchTargetInfo();
    fn LLVMInitializeMSP430TargetInfo();
    fn LLVMInitializeMipsTargetInfo();
    fn LLVMInitializeNVPTXTargetInfo();
    fn LLVMInitializePowerPCTargetInfo();
    fn LLVMInitializeRISCVTargetInfo();
    fn LLVMInitializeSparcTargetInfo();
    fn LLVMInitializeSystemZTargetInfo();
    fn LLVMInitializeVETargetInfo();
    fn LLVMInitializeWebAssemblyTargetInfo();
    fn LLVMInitializeX86TargetInfo();
    fn LLVMInitializeXCoreTargetInfo();

    fn LLVMInitializeAArch64Target();
    fn LLVMInitializeAMDGPUTarget();
    fn LLVMInitializeARMTarget();
    fn LLVMInitializeAVRTarget();
    fn LLVMInitializeBPFTarget();
    fn LLVMInitializeHexagonTarget();
    fn LLVMInitializeLanaiTarget();
    fn LLVMInitializeLoongArchTarget();
    fn LLVMInitializeMSP430Target();
    fn LLVMInitializeMipsTarget();
    fn LLVMInitializeNVPTXTarget();
    fn LLVMInitializePowerPCTarget();
    fn LLVMInitializeRISCVTarget();
    fn LLVMInitializeSparcTarget();
    fn LLVMInitializeSystemZTarget();
    fn LLVMInitializeVETarget();
    fn LLVMInitializeWebAssemblyTarget();
    fn LLVMInitializeX86Target();
    fn LLVMInitializeXCoreTarget();

    fn LLVMInitializeAArch64TargetMC();
    fn LLVMInitializeAMDGPUTargetMC();
    fn LLVMInitializeARMTargetMC();
    fn LLVMInitializeAVRTargetMC();
    fn LLVMInitializeBPFTargetMC();
    fn LLVMInitializeHexagonTargetMC();
    fn LLVMInitializeLanaiTargetMC();
    fn LLVMInitializeLoongArchTargetMC();
    fn LLVMInitializeMSP430TargetMC();
    fn LLVMInitializeMipsTargetMC();
    fn LLVMInitializeNVPTXTargetMC();
    fn LLVMInitializePowerPCTargetMC();
    fn LLVMInitializeRISCVTargetMC();
    fn LLVMInitializeSparcTargetMC();
    fn LLVMInitializeSystemZTargetMC();
    fn LLVMInitializeVETargetMC();
    fn LLVMInitializeWebAssemblyTargetMC();
    fn LLVMInitializeX86TargetMC();
    fn LLVMInitializeXCoreTargetMC();

    fn LLVMInitializeAArch64AsmPrinter();
    fn LLVMInitializeAMDGPUAsmPrinter();
    fn LLVMInitializeARMAsmPrinter();
    fn LLVMInitializeAVRAsmPrinter();
    fn LLVMInitializeBPFAsmPrinter();
    fn LLVMInitializeHexagonAsmPrinter();
    fn LLVMInitializeLanaiAsmPrinter();
    fn LLVMInitializeLoongArchAsmPrinter();
    fn LLVMInitializeMSP430AsmPrinter();
    fn LLVMInitializeMipsAsmPrinter();
    fn LLVMInitializeNVPTXAsmPrinter();
    fn LLVMInitializePowerPCAsmPrinter();
    fn LLVMInitializeRISCVAsmPrinter();
    fn LLVMInitializeSparcAsmPrinter();
    fn LLVMInitializeSystemZAsmPrinter();
    fn LLVMInitializeVEAsmPrinter();
    fn LLVMInitializeWebAssemblyAsmPrinter();
    fn LLVMInitializeX86AsmPrinter();
    fn LLVMInitializeXCoreAsmPrinter();

    fn LLVMInitializeAArch64AsmParser();
    fn LLVMInitializeAMDGPUAsmParser();
    fn LLVMInitializeARMAsmParser();
    fn LLVMInitializeAVRAsmParser();
    fn LLVMInitializeBPFAsmParser();
    fn LLVMInitializeHexagonAsmParser();
    fn LLVMInitializeLanaiAsmParser();
    fn LLVMInitializeLoongArchAsmParser();
    fn LLVMInitializeMSP430AsmParser();
    fn LLVMInitializeMipsAsmParser();
    fn LLVMInitializeNVPTXAsmParser();
    fn LLVMInitializePowerPCAsmParser();
    fn LLVMInitializeRISCVAsmParser();
    fn LLVMInitializeSparcAsmParser();
    fn LLVMInitializeSystemZAsmParser();
    fn LLVMInitializeVEAsmParser();
    fn LLVMInitializeWebAssemblyAsmParser();
    fn LLVMInitializeX86AsmParser();
    fn LLVMInitializeXCoreAsmParser();

    fn LLVMInitializeAArch64Disassembler();
    fn LLVMInitializeAMDGPUDisassembler();
    fn LLVMInitializeARMDisassembler();
    fn LLVMInitializeAVRDisassembler();
    fn LLVMInitializeBPFDisassembler();
    fn LLVMInitializeHexagonDisassembler();
    fn LLVMInitializeLanaiDisassembler();
    fn LLVMInitializeLoongArchDisassembler();
    fn LLVMInitializeMSP430Disassembler();
    fn LLVMInitializeMipsDisassembler();
    fn LLVMInitializeNVPTXDisassembler();
    fn LLVMInitializePowerPCDisassembler();
    fn LLVMInitializeRISCVDisassembler();
    fn LLVMInitializeSparcDisassembler();
    fn LLVMInitializeSystemZDisassembler();
    fn LLVMInitializeVEDisassembler();
    fn LLVMInitializeWebAssemblyDisassembler();
    fn LLVMInitializeX86Disassembler();
    fn LLVMInitializeXCoreDisassembler();
}

macro_rules! agg {
    ($name:ident, $($fns:ident),+ $(,)?) => {
        #[no_mangle]
        pub extern "C" fn $name() {
            unsafe { $( $fns(); )+ }
        }
    };
}

agg!(
    LLVM_InitializeAllTargetInfos,
    LLVMInitializeAArch64TargetInfo,
    LLVMInitializeAMDGPUTargetInfo,
    LLVMInitializeARMTargetInfo,
    LLVMInitializeAVRTargetInfo,
    LLVMInitializeBPFTargetInfo,
    LLVMInitializeHexagonTargetInfo,
    LLVMInitializeLanaiTargetInfo,
    LLVMInitializeLoongArchTargetInfo,
    LLVMInitializeMSP430TargetInfo,
    LLVMInitializeMipsTargetInfo,
    LLVMInitializeNVPTXTargetInfo,
    LLVMInitializePowerPCTargetInfo,
    LLVMInitializeRISCVTargetInfo,
    LLVMInitializeSparcTargetInfo,
    LLVMInitializeSystemZTargetInfo,
    LLVMInitializeVETargetInfo,
    LLVMInitializeWebAssemblyTargetInfo,
    LLVMInitializeX86TargetInfo,
    LLVMInitializeXCoreTargetInfo
);
agg!(
    LLVM_InitializeAllTargets,
    LLVMInitializeAArch64Target,
    LLVMInitializeAMDGPUTarget,
    LLVMInitializeARMTarget,
    LLVMInitializeAVRTarget,
    LLVMInitializeBPFTarget,
    LLVMInitializeHexagonTarget,
    LLVMInitializeLanaiTarget,
    LLVMInitializeLoongArchTarget,
    LLVMInitializeMSP430Target,
    LLVMInitializeMipsTarget,
    LLVMInitializeNVPTXTarget,
    LLVMInitializePowerPCTarget,
    LLVMInitializeRISCVTarget,
    LLVMInitializeSparcTarget,
    LLVMInitializeSystemZTarget,
    LLVMInitializeVETarget,
    LLVMInitializeWebAssemblyTarget,
    LLVMInitializeX86Target,
    LLVMInitializeXCoreTarget
);
agg!(
    LLVM_InitializeAllTargetMCs,
    LLVMInitializeAArch64TargetMC,
    LLVMInitializeAMDGPUTargetMC,
    LLVMInitializeARMTargetMC,
    LLVMInitializeAVRTargetMC,
    LLVMInitializeBPFTargetMC,
    LLVMInitializeHexagonTargetMC,
    LLVMInitializeLanaiTargetMC,
    LLVMInitializeLoongArchTargetMC,
    LLVMInitializeMSP430TargetMC,
    LLVMInitializeMipsTargetMC,
    LLVMInitializeNVPTXTargetMC,
    LLVMInitializePowerPCTargetMC,
    LLVMInitializeRISCVTargetMC,
    LLVMInitializeSparcTargetMC,
    LLVMInitializeSystemZTargetMC,
    LLVMInitializeVETargetMC,
    LLVMInitializeWebAssemblyTargetMC,
    LLVMInitializeX86TargetMC,
    LLVMInitializeXCoreTargetMC
);
agg!(
    LLVM_InitializeAllAsmPrinters,
    LLVMInitializeAArch64AsmPrinter,
    LLVMInitializeAMDGPUAsmPrinter,
    LLVMInitializeARMAsmPrinter,
    LLVMInitializeAVRAsmPrinter,
    LLVMInitializeBPFAsmPrinter,
    LLVMInitializeHexagonAsmPrinter,
    LLVMInitializeLanaiAsmPrinter,
    LLVMInitializeLoongArchAsmPrinter,
    LLVMInitializeMSP430AsmPrinter,
    LLVMInitializeMipsAsmPrinter,
    LLVMInitializeNVPTXAsmPrinter,
    LLVMInitializePowerPCAsmPrinter,
    LLVMInitializeRISCVAsmPrinter,
    LLVMInitializeSparcAsmPrinter,
    LLVMInitializeSystemZAsmPrinter,
    LLVMInitializeVEAsmPrinter,
    LLVMInitializeWebAssemblyAsmPrinter,
    LLVMInitializeX86AsmPrinter,
    LLVMInitializeXCoreAsmPrinter
);
// NB: the official Windows LLVM-C.dll has no AsmParser for NVPTX or XCore and
// no Disassembler for NVPTX; other platforms export them.
agg!(
    LLVM_InitializeAllAsmParsers,
    LLVMInitializeAArch64AsmParser,
    LLVMInitializeAMDGPUAsmParser,
    LLVMInitializeARMAsmParser,
    LLVMInitializeAVRAsmParser,
    LLVMInitializeBPFAsmParser,
    LLVMInitializeHexagonAsmParser,
    LLVMInitializeLanaiAsmParser,
    LLVMInitializeLoongArchAsmParser,
    LLVMInitializeMSP430AsmParser,
    LLVMInitializeMipsAsmParser,
    LLVMInitializePowerPCAsmParser,
    LLVMInitializeRISCVAsmParser,
    LLVMInitializeSparcAsmParser,
    LLVMInitializeSystemZAsmParser,
    LLVMInitializeVEAsmParser,
    LLVMInitializeWebAssemblyAsmParser,
    LLVMInitializeX86AsmParser
);
agg!(
    LLVM_InitializeAllDisassemblers,
    LLVMInitializeAArch64Disassembler,
    LLVMInitializeAMDGPUDisassembler,
    LLVMInitializeARMDisassembler,
    LLVMInitializeAVRDisassembler,
    LLVMInitializeBPFDisassembler,
    LLVMInitializeHexagonDisassembler,
    LLVMInitializeLanaiDisassembler,
    LLVMInitializeLoongArchDisassembler,
    LLVMInitializeMSP430Disassembler,
    LLVMInitializeMipsDisassembler,
    LLVMInitializePowerPCDisassembler,
    LLVMInitializeRISCVDisassembler,
    LLVMInitializeSparcDisassembler,
    LLVMInitializeSystemZDisassembler,
    LLVMInitializeVEDisassembler,
    LLVMInitializeWebAssemblyDisassembler,
    LLVMInitializeX86Disassembler
);

/// `LLVMBool` is `i32` (0 = success).
#[no_mangle]
pub extern "C" fn LLVM_InitializeNativeTarget() -> i32 {
    unsafe {
        LLVMInitializeX86TargetInfo();
        LLVMInitializeX86Target();
        LLVMInitializeX86TargetMC();
    }
    0
}

#[no_mangle]
pub extern "C" fn LLVM_InitializeNativeAsmPrinter() -> i32 {
    unsafe {
        LLVMInitializeX86AsmPrinter();
    }
    0
}

#[no_mangle]
pub extern "C" fn LLVM_InitializeNativeAsmParser() -> i32 {
    unsafe {
        LLVMInitializeX86AsmParser();
    }
    0
}

#[no_mangle]
pub extern "C" fn LLVM_InitializeNativeDisassembler() -> i32 {
    unsafe {
        LLVMInitializeX86Disassembler();
    }
    0
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
        OptimizationLevel::Default
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
    tm.write_to_file(module, FileType::Object, out)
        .map_err(|e| format!("cannot emit '{triple}' object: {e:?}"))
}

//! Processor features the crossing needs.

use core::arch::x86_64::{__cpuid, __cpuid_count};
use std::sync::LazyLock;

/// Size of the XSAVE area for the features the kernel enabled in XCR0.
static XSAVE_SIZE: LazyLock<usize> =
    LazyLock::new(|| (__cpuid_count(0xd, 0).ebx as usize).max(576));

/// Refuses to start without FSGSBASE (the stub swaps FS with WRFSBASE) or XSAVE enabled by the
/// kernel (the stub saves vector state with XSAVE). Returns a reason.
pub fn check() -> Result<(), &'static str> {
    let leaf1 = __cpuid(1);
    if leaf1.ecx & (1 << 27) == 0 {
        return Err("the processor or kernel offers no XSAVE (OSXSAVE is clear)");
    }
    let leaf7 = __cpuid_count(7, 0);
    if leaf7.ebx & 1 == 0 {
        return Err("the processor has no FSGSBASE instructions (with QEMU, use -cpu max or KVM)");
    }
    Ok(())
}

pub fn xsave_size() -> usize {
    *XSAVE_SIZE
}

pub fn xcr0() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { core::arch::asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") hi) };
    (hi as u64) << 32 | lo as u64
}

pub fn rdfsbase() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("rdfsbase {}", out(reg) v) };
    v
}

pub fn rdgsbase() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("rdgsbase {}", out(reg) v) };
    v
}

pub fn wrgsbase(v: u64) {
    unsafe { core::arch::asm!("wrgsbase {}", in(reg) v) };
}

/// AT_HWCAP: CPUID leaf 1 EDX, as Linux reports it on x86.
pub fn hwcap() -> u64 {
    __cpuid(1).edx as u64
}

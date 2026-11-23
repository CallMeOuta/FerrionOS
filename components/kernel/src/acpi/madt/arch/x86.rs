use core::{
    hint,
    sync::atomic::{AtomicU8, Ordering},
};

use crate::{
    arch::{
        device::local_apic::{the_local_apic, ApicId},
        start::{kstart_ap, KernelArgsAp},
    },
    context::context::Kstack,
    cpu_set::{LogicalCpuId, MAX_CPU_COUNT},
    memory::{
        Frame, KernelMapper, Page, PageFlags, PhysicalAddress, RmmA, RmmArch, VirtualAddress,
        PAGE_SIZE,
    },
    startup::AP_READY,
};

use super::{Madt, MadtEntry};

const TRAMPOLINE: usize = 0x8000;
static TRAMPOLINE_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trampoline"));
const MAX_AP_START_SPINS: usize = 100_000_000;

pub(super) fn init(madt: Madt) {
    let local_apic = unsafe { the_local_apic() };
    let me = local_apic.id();
    local_apic.configure_nmi(madt);

    let mut processors = [u32::MAX; MAX_CPU_COUNT as usize];
    processors[0] = me.get();
    let mut processor_count = 1;
    for entry in madt.iter() {
        let (id, flags) = match entry {
            MadtEntry::LocalApic(apic) => (u32::from(apic.id), apic.flags),
            MadtEntry::LocalX2Apic(apic) => (apic.id, apic.flags),
            _ => continue,
        };
        if flags & 1 == 0 || id == u32::MAX || processors[..processor_count].contains(&id) {
            continue;
        }
        assert!(
            local_apic.x2 || id < u32::from(u8::MAX),
            "APIC destination requires x2APIC"
        );
        assert!(processor_count < processors.len(), "MADT exceeds CPU limit");
        processors[processor_count] = id;
        processor_count += 1;
    }

    if local_apic.x2 {
        debug!("    X2APIC {}", me.get());
    } else {
        debug!("    XAPIC {}: {:>08X}", me.get(), local_apic.address);
    }

    if cfg!(not(feature = "multi_core")) {
        return;
    }

    // Map trampoline
    let trampoline_frame = Frame::containing(PhysicalAddress::new(TRAMPOLINE));
    let trampoline_page = Page::containing_address(VirtualAddress::new(TRAMPOLINE));
    let (result, page_table_physaddr) = unsafe {
        //TODO: do not have writable and executable!
        let mut mapper = KernelMapper::lock_rw();

        let result = mapper
            .map_phys(
                trampoline_page.start_address(),
                trampoline_frame.base(),
                PageFlags::new().execute(true).write(true),
                0,
            )
            .expect("failed to map trampoline");

        (result, mapper.table().phys().data())
    };
    result.flush();

    // Write trampoline, make sure TRAMPOLINE page is free for use
    for (i, val) in TRAMPOLINE_DATA.iter().enumerate() {
        unsafe {
            (*((TRAMPOLINE as *mut u8).add(i) as *const AtomicU8)).store(*val, Ordering::SeqCst);
        }
    }

    unsafe {
        crate::profiling::allocate(processor_count as u32);
    }
    for (index, &apic_id) in processors[..processor_count].iter().enumerate().skip(1) {
        assert!(!processors[..index].contains(&apic_id));
        let cpu_id = LogicalCpuId::next();

        // Allocate a stack
        let kstack = Kstack::new(Some(apic_id)).expect("no more frames in acpi stack_start");
        let stack_end = kstack.initial_top() as usize;

        let pcr_ptr = crate::arch::gdt::allocate_and_init_pcr(cpu_id, stack_end);

        let idt_ptr = crate::arch::idt::allocate_and_init_idt(cpu_id, ApicId::new(apic_id));

        let mut args = KernelArgsAp {
            stack_end: stack_end as *mut u8,
            kstack: Some(kstack),
            cpu_id,
            pcr_ptr,
            idt_ptr,
        };

        let ap_ready = (TRAMPOLINE + 8) as *mut u64;
        let ap_args_ptr = unsafe { ap_ready.add(1) };
        let ap_page_table = unsafe { ap_ready.add(2) };
        let ap_code = unsafe { ap_ready.add(3) };

        // Set the ap_ready to 0, volatile
        unsafe {
            ap_ready.write(0);
            ap_args_ptr.write(&mut args as *mut _ as u64);
            ap_page_table.write(page_table_physaddr as u64);
            ap_code.write(kstart_ap as *const () as u64);

            // TODO: Is this necessary (this fence)?
            core::arch::asm!("");
        };
        AP_READY.store(false, Ordering::SeqCst);

        // Send INIT IPI
        {
            let mut icr = 0x4500;
            if local_apic.x2 {
                icr |= u64::from(apic_id) << 32;
            } else {
                icr |= u64::from(apic_id) << 56;
            }
            local_apic.set_icr(icr);
        }

        // Send START IPI
        {
            let ap_segment = (TRAMPOLINE >> 12) & 0xFF;
            let mut icr = 0x4600 | ap_segment as u64;

            if local_apic.x2 {
                icr |= u64::from(apic_id) << 32;
            } else {
                icr |= u64::from(apic_id) << 56;
            }

            local_apic.set_icr(icr);
        }

        // Wait for trampoline ready
        let mut ready = false;
        for _ in 0..MAX_AP_START_SPINS {
            if unsafe { (*ap_ready.cast::<AtomicU8>()).load(Ordering::SeqCst) } != 0
                && AP_READY.load(Ordering::SeqCst)
            {
                ready = true;
                break;
            }
            hint::spin_loop();
        }
        assert!(ready, "application processor did not start");

        RmmA::invalidate_all();
    }

    // Unmap trampoline
    let (_frame, lvl, _, flush) = unsafe {
        KernelMapper::lock_rw()
            .unmap_phys(trampoline_page.start_address())
            .expect("failed to unmap trampoline page")
    };
    assert_eq!(lvl, 0, "not mapped with huge pages");
    flush.flush();
}

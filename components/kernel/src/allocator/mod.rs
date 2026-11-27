use crate::memory::{deallocate_frame, Frame, KernelMapper, Page, PageFlags, RmmA, VirtualAddress};
use rmm::{Flusher, FrameAllocator, PageFlushAll};

pub use self::linked_list::Allocator;
mod linked_list;

/// Size of kernel heap
const KERNEL_HEAP_SIZE: usize = ::rmm::MEGABYTE;

unsafe fn rollback_heap_pages(
    mapper: &mut KernelMapper<true>,
    start: Page,
    mapped_count: usize,
    mut flush_all: PageFlushAll<RmmA>,
) {
    for i in 0..mapped_count {
        let page = start.next_by(i);
        if let Some((phys, lvl, _, flush)) = unsafe { mapper.unmap_phys(page.start_address()) } {
            assert_eq!(lvl, 0, "kernel heap page mapped as huge page");
            flush_all.consume(flush);
            unsafe {
                deallocate_frame(Frame::containing(phys));
            }
        }
    }
    flush_all.flush();
}

unsafe fn map_heap(mapper: &mut KernelMapper<true>, offset: usize, size: usize) -> Result<(), ()> {
    let mut flush_all = PageFlushAll::new();

    let heap_start_page = Page::containing_address(VirtualAddress::new(offset));
    let heap_end_page = Page::containing_address(VirtualAddress::new(offset + size - 1));
    let mut mapped_count = 0usize;
    for page in Page::range_inclusive(heap_start_page, heap_end_page) {
        let Some(phys) = mapper.allocator_mut().allocate_one() else {
            unsafe {
                rollback_heap_pages(mapper, heap_start_page, mapped_count, flush_all);
            }
            return Err(());
        };
        let Some(flush) = (unsafe {
            mapper.map_phys(
                page.start_address(),
                phys,
                PageFlags::new().write(true).global(true),
                0,
            )
        }) else {
            unsafe {
                deallocate_frame(Frame::containing(phys));
                rollback_heap_pages(mapper, heap_start_page, mapped_count, flush_all);
            }
            return Err(());
        };
        flush_all.consume(flush);
        mapped_count += 1;
    }

    flush_all.flush();
    Ok(())
}

pub unsafe fn init() {
    unsafe {
        let offset = crate::kernel_heap_offset();
        let size = KERNEL_HEAP_SIZE;

        // Map heap pages
        if map_heap(&mut KernelMapper::lock_rw(), offset, size).is_err() {
            panic!("OUT OF MEMORY: failed to map initial kernel heap");
        }

        // Initialize global heap
        Allocator::init(offset, size);
    }
}

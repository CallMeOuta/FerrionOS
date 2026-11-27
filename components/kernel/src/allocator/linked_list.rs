use crate::memory::KernelMapper;
use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};
use linked_list_allocator::Heap;
use spin::Mutex;

static HEAP: Mutex<Option<Heap>> = Mutex::new(None);
const MAX_HEAP_GROW_ATTEMPTS: usize = 64;

pub struct Allocator;

impl Allocator {
    pub unsafe fn init(offset: usize, size: usize) {
        unsafe {
            *HEAP.lock() = Some(Heap::new(offset, size));
        }
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let max_attempts = (layout.size() / super::KERNEL_HEAP_SIZE)
            .saturating_add(2)
            .min(MAX_HEAP_GROW_ATTEMPTS);
        unsafe {
            let mut heap_guard = HEAP.lock();
            let Some(ref mut heap) = *heap_guard else {
                panic!("__rust_allocate: heap not initialized");
            };
            for _ in 0..max_attempts {
                if let Ok(ptr) = heap.allocate_first_fit(layout) {
                    return ptr.as_ptr();
                }
                let size = heap.size();
                if super::map_heap(
                    &mut KernelMapper::lock_rw(),
                    crate::kernel_heap_offset() + size,
                    super::KERNEL_HEAP_SIZE,
                )
                .is_err()
                {
                    return core::ptr::null_mut();
                }
                heap.extend(super::KERNEL_HEAP_SIZE);
            }
            heap.allocate_first_fit(layout)
                .map_or(core::ptr::null_mut(), |ptr| ptr.as_ptr())
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            HEAP.lock()
                .as_mut()
                .expect("heap not initialized")
                .deallocate(NonNull::new_unchecked(ptr), layout)
        }
    }
}

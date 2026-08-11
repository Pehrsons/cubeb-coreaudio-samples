//! An allocator that zeroes every allocation.
//!
//! Allocators differ in what they leave in freshly allocated memory, and some zero it. Code that
//! reads uninitialized memory therefore behaves differently depending on the allocator, so turning
//! a zeroing one on is a way to test whether a measured behaviour depends on that.
//!
//! Note the limits: this covers allocations made by Rust only. Anything the system audio frameworks
//! or libcubeb's C and C++ allocate goes through libmalloc and is unaffected, as is uninitialized
//! stack memory. `src/zeroing_malloc.c` covers the rest of the process.

use std::alloc::{GlobalAlloc, Layout, System};

pub struct ZeroingAlloc;

unsafe impl GlobalAlloc for ZeroingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        System.alloc_zeroed(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        System.alloc_zeroed(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // System::realloc leaves the grown tail uninitialized, so allocate zeroed and copy.
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let new_ptr = System.alloc_zeroed(new_layout);
        if !new_ptr.is_null() {
            std::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
            System.dealloc(ptr, layout);
        }
        new_ptr
    }
}

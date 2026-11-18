//! Dynamic Kernel Heap Allocator for Project Genesis.
//!
//! Provides a lean, robust free-list allocator implementing `core::alloc::GlobalAlloc`
//! with spinlock synchronization, block splitting, and contiguous address-sorted merging.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        #[cfg(target_os = "uefi")]
        let saved_daif = unsafe {
            let d: u64;
            core::arch::asm!("mrs {}, daif", out(reg) d, options(nomem, nostack));
            core::arch::asm!("msr daifset, #2", options(nomem, nostack));
            d
        };

        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        SpinLockGuard {
            lock: self,
            #[cfg(target_os = "uefi")]
            saved_daif,
        }
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    #[cfg(target_os = "uefi")]
    saved_daif: u64,
}

impl<'a, T> core::ops::Deref for SpinLockGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T> core::ops::DerefMut for SpinLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<'a, T> Drop for SpinLockGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        #[cfg(target_os = "uefi")]
        unsafe {
            core::arch::asm!("msr daif, {}", in(reg) self.saved_daif, options(nomem, nostack));
        }
    }
}

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

#[repr(C, align(16))]
struct ChunkHeader {
    size: usize,
    _pad: usize,
}

pub struct FreeListHeap {
    head: Option<NonNull<FreeBlock>>,
    total_bytes: usize,
    allocated_bytes: usize,
}

unsafe impl Send for FreeListHeap {}

impl FreeListHeap {
    pub const fn new() -> Self {
        Self {
            head: None,
            total_bytes: 0,
            allocated_bytes: 0,
        }
    }

    /// Adds a free memory region to the heap.
    pub unsafe fn add_region(&mut self, start: usize, size: usize) {
        let aligned_start = (start + 15) & !15;
        let end = start + size;
        if end <= aligned_start {
            return;
        }
        let aligned_size = (end - aligned_start) & !15;
        if aligned_size < core::mem::size_of::<FreeBlock>() {
            return;
        }

        let block = aligned_start as *mut FreeBlock;
        (*block).size = aligned_size;
        (*block).next = None;

        self.total_bytes += aligned_size;
        self.insert_and_merge(NonNull::new_unchecked(block));
    }

    /// Inserts a block into the sorted free list and merges adjacent contiguous blocks.
    unsafe fn insert_and_merge(&mut self, mut new_block: NonNull<FreeBlock>) {
        let new_addr = new_block.as_ptr() as usize;
        let new_size = new_block.as_ref().size;

        // If list is empty, insert as head
        let mut curr = match self.head {
            None => {
                self.head = Some(new_block);
                return;
            }
            Some(head) => head,
        };

        // If new_block comes before current head:
        if new_addr < curr.as_ptr() as usize {
            if new_addr + new_size == curr.as_ptr() as usize {
                // Merge with old head
                new_block.as_mut().size += curr.as_ref().size;
                new_block.as_mut().next = curr.as_ref().next;
            } else {
                new_block.as_mut().next = Some(curr);
            }
            self.head = Some(new_block);
            return;
        }

        // Find insertion point
        while let Some(next) = curr.as_ref().next {
            if (next.as_ptr() as usize) > new_addr {
                break;
            }
            curr = next;
        }

        let curr_next = curr.as_ref().next;

        // 1. Try to merge new_block with next
        if let Some(next) = curr_next {
            if new_addr + new_size == next.as_ptr() as usize {
                new_block.as_mut().size += next.as_ref().size;
                new_block.as_mut().next = next.as_ref().next;
            } else {
                new_block.as_mut().next = Some(next);
            }
        }

        // 2. Try to merge curr with new_block
        let curr_addr = curr.as_ptr() as usize;
        let curr_size = curr.as_ref().size;
        if curr_addr + curr_size == new_addr {
            curr.as_mut().size += new_block.as_ref().size;
            curr.as_mut().next = new_block.as_ref().next;
        } else {
            curr.as_mut().next = Some(new_block);
        }
    }

    /// Allocates memory satisfying `layout`.
    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(core::mem::align_of::<ChunkHeader>());
        let header_size = core::mem::size_of::<ChunkHeader>();
        let payload_size = (layout.size().max(1) + 15) & !15;

        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut current = self.head;

        while let Some(mut block) = current {
            let block_addr = block.as_ptr() as usize;
            let block_size = block.as_ref().size;

            let raw_payload = block_addr + header_size;
            let aligned_payload = (raw_payload + align - 1) & !(align - 1);
            let chunk_start = aligned_payload - header_size;
            let prefix_size = chunk_start - block_addr;
            let total_needed = prefix_size + header_size + payload_size;

            if block_size >= total_needed {
                let remaining_after = block_size - total_needed;
                let next_free = block.as_ref().next;

                // prefix_size is either 0 or a multiple of 16 (since block_addr and chunk_start are 16-byte aligned).
                if prefix_size >= core::mem::size_of::<FreeBlock>() {
                    block.as_mut().size = prefix_size;
                    if remaining_after >= core::mem::size_of::<FreeBlock>() {
                        let remainder_addr = chunk_start + header_size + payload_size;
                        let remainder_ptr = remainder_addr as *mut FreeBlock;
                        (*remainder_ptr).size = remaining_after;
                        (*remainder_ptr).next = next_free;
                        block.as_mut().next = NonNull::new(remainder_ptr);
                    } else {
                        block.as_mut().next = next_free;
                    }
                } else {
                    // Entire front of block used
                    if remaining_after >= core::mem::size_of::<FreeBlock>() {
                        let remainder_addr = block_addr + total_needed;
                        let remainder_ptr = remainder_addr as *mut FreeBlock;
                        (*remainder_ptr).size = remaining_after;
                        (*remainder_ptr).next = next_free;

                        if let Some(mut p) = prev {
                            p.as_mut().next = NonNull::new(remainder_ptr);
                        } else {
                            self.head = NonNull::new(remainder_ptr);
                        }
                    } else {
                        // Entire block consumed
                        if let Some(mut p) = prev {
                            p.as_mut().next = next_free;
                        } else {
                            self.head = next_free;
                        }
                    }
                }

                // Write ChunkHeader. When remaining_after < size_of::<FreeBlock>(),
                // add remaining_after to chunk size so tail bytes are never leaked!
                let header = chunk_start as *mut ChunkHeader;
                let mut chunk_size = header_size + payload_size;
                if remaining_after < core::mem::size_of::<FreeBlock>() {
                    chunk_size += remaining_after;
                }
                (*header).size = chunk_size;

                self.allocated_bytes += chunk_size;
                return aligned_payload as *mut u8;
            }

            prev = Some(block);
            current = block.as_ref().next;
        }

        core::ptr::null_mut()
    }

    /// Frees previously allocated memory.
    pub unsafe fn dealloc(&mut self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let header_size = core::mem::size_of::<ChunkHeader>();
        let header = (ptr as usize - header_size) as *mut ChunkHeader;
        let total_size = (*header).size;

        if total_size >= header_size {
            self.allocated_bytes = self.allocated_bytes.saturating_sub(total_size);
            self.add_region(header as usize, total_size);
        }
    }
}

pub struct LockedHeap {
    inner: SpinLock<FreeListHeap>,
}

impl LockedHeap {
    pub const fn empty() -> Self {
        Self {
            inner: SpinLock::new(FreeListHeap::new()),
        }
    }

    pub unsafe fn init(&self, start: usize, size: usize) {
        self.inner.lock().add_region(start, size);
    }

    pub fn total_bytes(&self) -> usize {
        self.inner.lock().total_bytes
    }

    pub fn allocated_bytes(&self) -> usize {
        self.inner.lock().allocated_bytes
    }
}

#[cfg(target_os = "uefi")]
#[global_allocator]
pub static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();

#[cfg(not(target_os = "uefi"))]
pub static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.inner.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inner.lock().dealloc(ptr, layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec::Vec;

    #[test]
    fn free_list_alloc_and_free_cycle() {
        #[repr(align(16))]
        struct AlignedBuf<const N: usize>([u8; N]);
        let mut buffer = AlignedBuf([0u8; 16384]);
        let mut heap = FreeListHeap::new();
        unsafe {
            heap.add_region(buffer.0.as_mut_ptr() as usize, buffer.0.len());
        }

        assert_eq!(heap.total_bytes, 16384);
        assert_eq!(heap.allocated_bytes, 0);

        // Allocate small block
        let layout1 = Layout::from_size_align(64, 8).unwrap();
        let p1 = unsafe { heap.alloc(layout1) };
        assert!(!p1.is_null());
        assert_eq!(p1 as usize % 8, 0);

        // Allocate multi-page block (8 KiB)
        let layout2 = Layout::from_size_align(8192, 16).unwrap();
        let p2 = unsafe { heap.alloc(layout2) };
        assert!(!p2.is_null());
        assert_eq!(p2 as usize % 16, 0);

        // Deallocate in reverse order
        unsafe {
            heap.dealloc(p2, layout2);
            heap.dealloc(p1, layout1);
        }

        assert_eq!(heap.allocated_bytes, 0);

        // Reallocate large block to confirm merging worked
        let layout3 = Layout::from_size_align(12000, 16).unwrap();
        let p3 = unsafe { heap.alloc(layout3) };
        assert!(!p3.is_null());

        unsafe {
            heap.dealloc(p3, layout3);
        }
        assert_eq!(heap.allocated_bytes, 0);
    }

    #[test]
    fn odd_sized_and_high_alignment_cycles_and_coalescing() {
        #[repr(align(16))]
        struct AlignedBuf<const N: usize>([u8; N]);
        let mut buffer = AlignedBuf([0u8; 32768]);
        let mut heap = FreeListHeap::new();
        unsafe {
            heap.add_region(buffer.0.as_mut_ptr() as usize, buffer.0.len());
        }

        let initial_total = heap.total_bytes;
        assert_eq!(initial_total, 32768);
        assert_eq!(heap.allocated_bytes, 0);

        // Odd sizes: 1, 3, 17, 33, 99, 137
        let odd_layouts = [
            Layout::from_size_align(1, 1).unwrap(),
            Layout::from_size_align(3, 1).unwrap(),
            Layout::from_size_align(17, 1).unwrap(),
            Layout::from_size_align(33, 8).unwrap(),
            Layout::from_size_align(99, 1).unwrap(),
            Layout::from_size_align(137, 4).unwrap(),
        ];

        let mut odd_ptrs = Vec::new();
        for layout in &odd_layouts {
            let ptr = unsafe { heap.alloc(*layout) };
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % layout.align(), 0);
            odd_ptrs.push(ptr);
        }

        // High alignment: align = 64
        let layout_align64 = Layout::from_size_align(256, 64).unwrap();
        let p_align64 = unsafe { heap.alloc(layout_align64) };
        assert!(!p_align64.is_null());
        assert_eq!(p_align64 as usize % 64, 0);

        // High alignment: align = 4096
        let layout_align4096 = Layout::from_size_align(4096, 4096).unwrap();
        let p_align4096 = unsafe { heap.alloc(layout_align4096) };
        assert!(!p_align4096.is_null());
        assert_eq!(p_align4096 as usize % 4096, 0);

        // Deallocate high alignment first
        unsafe {
            heap.dealloc(p_align4096, layout_align4096);
            heap.dealloc(p_align64, layout_align64);
        }

        // Deallocate odd sizes in reverse
        for (ptr, layout) in odd_ptrs.into_iter().zip(odd_layouts.iter()).rev() {
            unsafe {
                heap.dealloc(ptr, *layout);
            }
        }

        // All bytes must be completely accounted for and coalesced
        assert_eq!(heap.allocated_bytes, 0);

        // Verify that after coalescing, an allocation using nearly the full heap succeeds
        let big_layout = Layout::from_size_align(initial_total - 256, 16).unwrap();
        let big_ptr = unsafe { heap.alloc(big_layout) };
        assert!(!big_ptr.is_null());

        unsafe {
            heap.dealloc(big_ptr, big_layout);
        }
        assert_eq!(heap.allocated_bytes, 0);
    }
}

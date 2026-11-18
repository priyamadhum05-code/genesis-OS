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
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinLockGuard { lock: self }
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
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
        if size < core::mem::size_of::<FreeBlock>() {
            return;
        }

        let block = start as *mut FreeBlock;
        (*block).size = size;
        (*block).next = None;

        self.total_bytes += size;
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
        let payload_size = layout.size().max(1);

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

                // If there's an aligned prefix large enough to be a free block
                if prefix_size >= core::mem::size_of::<FreeBlock>() {
                    block.as_mut().size = prefix_size;
                    // Remainder after chunk
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

                // Write ChunkHeader
                let header = chunk_start as *mut ChunkHeader;
                let chunk_size = header_size + payload_size + if prefix_size < core::mem::size_of::<FreeBlock>() { prefix_size } else { 0 };
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

    #[test]
    fn free_list_alloc_and_free_cycle() {
        let mut buffer = [0u8; 16384];
        let mut heap = FreeListHeap::new();
        unsafe {
            heap.add_region(buffer.as_mut_ptr() as usize, buffer.len());
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
}

//! Memory Management Subsystem for Project Genesis.

pub mod heap;

pub use heap::{FreeListHeap, LockedHeap, HEAP_ALLOCATOR};

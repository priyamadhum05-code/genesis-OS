//! Device drivers for Project Genesis.

pub mod virtio_blk;

pub use virtio_blk::{VirtIOBlockDevice, VIRTIO_DEV_BLOCK, VIRTIO_MAGIC, VIRTIO_MMIO_BASE};

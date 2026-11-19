//! Device drivers for Project Genesis.

pub mod fb;
pub mod virtio_blk;
pub mod virtio_input;
pub mod virtio_net;

pub use fb::{
    global_fb_console_clear, global_fb_console_write, register_global_fb_console, FbConsole,
    Framebuffer,
};
pub use virtio_blk::{VirtIOBlockDevice, VIRTIO_DEV_BLOCK, VIRTIO_MAGIC, VIRTIO_MMIO_BASE};
pub use virtio_input::{VirtIOInputDevice, VirtioInputEvent, VIRTIO_DEV_INPUT};
pub use virtio_net::{VirtIONetDevice, VIRTIO_DEV_NET};

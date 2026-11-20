//! Project Genesis — Unified Block Device Trait & Subsystem (Stage 21).

use crate::drivers::{nvme::NvmeDevice, usb_msc::UsbMscDevice, virtio_blk::VirtIOBlockDevice};

pub trait BlockDevice {
    /// Read consecutive blocks starting at `lba` into buffer at `buf_pa` / `buf_va`.
    unsafe fn read_blocks(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str>;

    /// Write consecutive blocks starting at `lba` from buffer at `buf_pa` / `buf_va`.
    unsafe fn write_blocks(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str>;

    /// Return block/sector size in bytes (usually 512).
    fn block_size(&self) -> u32;

    /// Return total blocks/sectors.
    fn total_blocks(&self) -> u64;

    /// Return device name (e.g. "blk0", "nvme0", "usb0").
    fn name(&self) -> &str;

    /// Convenience alias for sector reading
    #[inline]
    unsafe fn read_sectors(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        self.read_blocks(lba, count, buf_pa, buf_va)
    }

    /// Convenience alias for sector writing
    #[inline]
    unsafe fn write_sectors(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        self.write_blocks(lba, count, buf_pa, buf_va)
    }
}

impl<T: BlockDevice + ?Sized> BlockDevice for &mut T {
    unsafe fn read_blocks(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        (**self).read_blocks(lba, count, buf_pa, buf_va)
    }

    unsafe fn write_blocks(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        (**self).write_blocks(lba, count, buf_pa, buf_va)
    }

    fn block_size(&self) -> u32 {
        (**self).block_size()
    }

    fn total_blocks(&self) -> u64 {
        (**self).total_blocks()
    }

    fn name(&self) -> &str {
        (**self).name()
    }
}

/// Unified Block Device Reference Enum (avoids vtable indirect calls across translation regimes)
pub enum AnyBlockDev<'a> {
    VirtIO(&'a mut VirtIOBlockDevice),
    Nvme(&'a mut NvmeDevice),
    Usb(&'a mut UsbMscDevice),
}

impl<'a> BlockDevice for AnyBlockDev<'a> {
    unsafe fn read_blocks(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        match self {
            AnyBlockDev::VirtIO(d) => d.read_blocks(lba, count, buf_pa, buf_va),
            AnyBlockDev::Nvme(d) => d.read_blocks(lba, count, buf_pa, buf_va),
            AnyBlockDev::Usb(d) => d.read_blocks(lba, count, buf_pa, buf_va),
        }
    }

    unsafe fn write_blocks(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        match self {
            AnyBlockDev::VirtIO(d) => d.write_blocks(lba, count, buf_pa, buf_va),
            AnyBlockDev::Nvme(d) => d.write_blocks(lba, count, buf_pa, buf_va),
            AnyBlockDev::Usb(d) => d.write_blocks(lba, count, buf_pa, buf_va),
        }
    }

    fn block_size(&self) -> u32 {
        match self {
            AnyBlockDev::VirtIO(d) => d.block_size(),
            AnyBlockDev::Nvme(d) => d.block_size(),
            AnyBlockDev::Usb(d) => d.block_size(),
        }
    }

    fn total_blocks(&self) -> u64 {
        match self {
            AnyBlockDev::VirtIO(d) => d.total_blocks(),
            AnyBlockDev::Nvme(d) => d.total_blocks(),
            AnyBlockDev::Usb(d) => d.total_blocks(),
        }
    }

    fn name(&self) -> &str {
        match self {
            AnyBlockDev::VirtIO(d) => d.name(),
            AnyBlockDev::Nvme(d) => d.name(),
            AnyBlockDev::Usb(d) => d.name(),
        }
    }
}

/// Query an active BlockDevice by name ("blk0", "nvme0", "usb0")
pub unsafe fn get_block_device_by_name(name: &str) -> Option<AnyBlockDev<'static>> {
    if name == "blk0" || name == "virtio-blk" {
        if let Some(ptr) = crate::drivers::virtio_blk::get_global_virtio_blk() {
            return Some(AnyBlockDev::VirtIO(&mut *ptr));
        }
    } else if name == "nvme0" || name == "nvme" {
        if let Some(dev) = crate::drivers::nvme::get_global_nvme() {
            if dev.is_initialized {
                return Some(AnyBlockDev::Nvme(dev));
            }
        }
    } else if name == "usb0" || name == "usb" {
        if let Some(dev) = crate::drivers::usb_msc::get_global_usb_msc() {
            if dev.is_initialized {
                return Some(AnyBlockDev::Usb(dev));
            }
        }
    }
    None
}

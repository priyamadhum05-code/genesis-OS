//! VirtIO Block MMIO Driver for Project Genesis.
//!
//! Provides device probing across QEMU `virt` MMIO slots (0x0A00_0000..0x0A00_3E00),
//! virtqueue initialization, and synchronous Sector 0 reads.

pub const VIRTIO_MMIO_BASE: u64 = 0x0A00_0000;
pub const VIRTIO_MMIO_STRIDE: u64 = 0x200;
pub const VIRTIO_MMIO_SLOTS: usize = 32;

pub const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"
pub const VIRTIO_DEV_BLOCK: u32 = 2;

pub const VIRTIO_REG_MAGIC: usize = 0x000;
pub const VIRTIO_REG_VERSION: usize = 0x004;
pub const VIRTIO_REG_DEVICE_ID: usize = 0x008;
pub const VIRTIO_REG_VENDOR_ID: usize = 0x00C;
pub const VIRTIO_REG_DEV_FEAT: usize = 0x010;
pub const VIRTIO_REG_DEV_FEAT_SEL: usize = 0x014;
pub const VIRTIO_REG_DRV_FEAT: usize = 0x020;
pub const VIRTIO_REG_DRV_FEAT_SEL: usize = 0x024;
pub const VIRTIO_REG_GUEST_PAGE_SIZE: usize = 0x028;
pub const VIRTIO_REG_QUEUE_SEL: usize = 0x030;
pub const VIRTIO_REG_QUEUE_NUM_MAX: usize = 0x034;
pub const VIRTIO_REG_QUEUE_NUM: usize = 0x038;
pub const VIRTIO_REG_QUEUE_ALIGN: usize = 0x03C;
pub const VIRTIO_REG_QUEUE_PFN: usize = 0x040;
pub const VIRTIO_REG_QUEUE_READY: usize = 0x044;
pub const VIRTIO_REG_QUEUE_NOTIFY: usize = 0x050;
pub const VIRTIO_REG_STATUS: usize = 0x070;
pub const VIRTIO_REG_QUEUE_DESC_LO: usize = 0x080;
pub const VIRTIO_REG_QUEUE_DESC_HI: usize = 0x084;
pub const VIRTIO_REG_QUEUE_DRV_LO: usize = 0x090;
pub const VIRTIO_REG_QUEUE_DRV_HI: usize = 0x094;
pub const VIRTIO_REG_QUEUE_DEV_LO: usize = 0x0A0;
pub const VIRTIO_REG_QUEUE_DEV_HI: usize = 0x0A4;

pub const VIRTQ_DESC_F_NEXT: u16 = 1;
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

#[repr(C, align(16))]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C, align(2))]
pub struct VirtqAvail<const N: usize> {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; N],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C, align(4))]
pub struct VirtqUsed<const N: usize> {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; N],
}

pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtIOBlkReqHeader {
    pub type_: u32,
    pub reserved: u32,
    pub sector: u64,
}

pub struct VirtIOBlockDevice {
    pub base_va: u64,
    pub version: u32,
    pub queue_size: u16,
    pub queue_pa: u64,
    pub queue_va: u64,
}

impl VirtIOBlockDevice {
    #[inline]
    unsafe fn read32(&self, offset: usize) -> u32 {
        core::ptr::read_volatile((self.base_va + offset as u64) as *const u32)
    }

    #[inline]
    unsafe fn write32(&self, offset: usize, val: u32) {
        core::ptr::write_volatile((self.base_va + offset as u64) as *mut u32, val);
    }

    /// Probes and initializes a VirtIO Block device at `base_va`.
    /// `queue_pa` / `queue_va` must point to an allocated, zeroed 4 KiB physical page.
    pub unsafe fn init(base_va: u64, queue_pa: u64, queue_va: u64) -> Result<Self, &'static str> {
        let mut dev = Self {
            base_va,
            version: 0,
            queue_size: 16,
            queue_pa,
            queue_va,
        };

        let magic = dev.read32(VIRTIO_REG_MAGIC);
        if magic != VIRTIO_MAGIC {
            return Err("Invalid VirtIO magic value");
        }

        let dev_id = dev.read32(VIRTIO_REG_DEVICE_ID);
        if dev_id != VIRTIO_DEV_BLOCK {
            return Err("Not a VirtIO block device");
        }

        let version = dev.read32(VIRTIO_REG_VERSION);
        dev.version = version;

        // 1. Reset device
        dev.write32(VIRTIO_REG_STATUS, 0);

        // 2. Set ACKNOWLEDGE (1)
        dev.write32(VIRTIO_REG_STATUS, 1);

        // 3. Set DRIVER (2)
        let status = dev.read32(VIRTIO_REG_STATUS);
        dev.write32(VIRTIO_REG_STATUS, status | 2);

        // 4. Feature negotiation
        if version >= 2 {
            dev.write32(VIRTIO_REG_DEV_FEAT_SEL, 0);
            let _f0 = dev.read32(VIRTIO_REG_DEV_FEAT);
            dev.write32(VIRTIO_REG_DEV_FEAT_SEL, 1);
            let f1 = dev.read32(VIRTIO_REG_DEV_FEAT);

            dev.write32(VIRTIO_REG_DRV_FEAT_SEL, 0);
            dev.write32(VIRTIO_REG_DRV_FEAT, 0);
            dev.write32(VIRTIO_REG_DRV_FEAT_SEL, 1);
            dev.write32(VIRTIO_REG_DRV_FEAT, f1 & 1); // VIRTIO_F_VERSION_1

            // Set FEATURES_OK (8)
            let cur = dev.read32(VIRTIO_REG_STATUS);
            dev.write32(VIRTIO_REG_STATUS, cur | 8);
            if (dev.read32(VIRTIO_REG_STATUS) & 8) == 0 {
                return Err("VirtIO features rejected by device");
            }
        }

        // 5. Configure Virtqueue 0
        dev.write32(VIRTIO_REG_QUEUE_SEL, 0);
        let max_q = dev.read32(VIRTIO_REG_QUEUE_NUM_MAX);
        if max_q == 0 {
            return Err("Virtqueue 0 not available");
        }
        let q_size = 16u16;
        dev.write32(VIRTIO_REG_QUEUE_NUM, q_size as u32);
        dev.queue_size = q_size;

        if version >= 2 {
            // Modern MMIO register layout:
            let desc_pa = queue_pa;
            let avail_pa = queue_pa + (16 * dev.queue_size as u64);
            let used_pa = queue_pa + 4096;

            dev.write32(VIRTIO_REG_QUEUE_DESC_LO, (desc_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DESC_HI, (desc_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_LO, (avail_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_HI, (avail_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_LO, (used_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_HI, (used_pa >> 32) as u32);

            dev.write32(VIRTIO_REG_QUEUE_READY, 1);
        } else {
            // Legacy MMIO register layout:
            dev.write32(VIRTIO_REG_GUEST_PAGE_SIZE, 4096);
            dev.write32(VIRTIO_REG_QUEUE_ALIGN, 4096);
            dev.write32(VIRTIO_REG_QUEUE_PFN, (queue_pa / 4096) as u32);
        }

        // 6. Set DRIVER_OK (128)
        let cur = dev.read32(VIRTIO_REG_STATUS);
        dev.write32(VIRTIO_REG_STATUS, cur | 128);

        Ok(dev)
    }

    /// Synchronously reads Sector 0 (512 bytes) from the block device.
    /// `req_pa` / `req_va` must point to an allocated 4 KiB physical page for the request buffers.
    pub unsafe fn read_sector_0(&self, req_pa: u64, req_va: u64) -> Result<u16, &'static str> {
        let q_num = self.queue_size as usize;
        let avail_offset = 16 * q_num;
        let used_offset = if self.version == 1 { 4096 } else { 4096 };

        // Zero out request buffers
        core::ptr::write_bytes(req_va as *mut u8, 0, 4096);

        // Header at req_va + 0x000 (16 bytes)
        let header = req_va as *mut VirtIOBlkReqHeader;
        (*header).type_ = VIRTIO_BLK_T_IN;
        (*header).reserved = 0;
        (*header).sector = 0;

        // Status byte at req_va + 0x300 (1 byte, initialized to 0xFF)
        core::ptr::write_volatile((req_va + 0x300) as *mut u8, 0xFF);

        // Populate descriptor chain at queue_va + 0x000
        let desc_table = self.queue_va as *mut VirtqDesc;

        // Desc 0: Header (Read by device)
        *desc_table.add(0) = VirtqDesc {
            addr: req_pa,
            len: 16,
            flags: VIRTQ_DESC_F_NEXT,
            next: 1,
        };

        // Desc 1: Data Buffer (Written by device)
        *desc_table.add(1) = VirtqDesc {
            addr: req_pa + 0x100,
            len: 512,
            flags: VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            next: 2,
        };

        // Desc 2: Status Byte (Written by device)
        *desc_table.add(2) = VirtqDesc {
            addr: req_pa + 0x300,
            len: 1,
            flags: VIRTQ_DESC_F_WRITE,
            next: 0,
        };

        // Add to Available ring at queue_va + avail_offset
        let avail = (self.queue_va + avail_offset as u64) as *mut VirtqAvail<128>;
        let cur_avail_idx = core::ptr::read_volatile(&(*avail).idx);
        core::ptr::write_volatile(
            &mut (*avail).ring[(cur_avail_idx as usize) % q_num],
            0, // Descriptor 0 is head
        );

        core::ptr::write_volatile(&mut (*avail).idx, cur_avail_idx.wrapping_add(1));

        #[cfg(target_os = "uefi")]
        {
            for p in (0..4096).step_by(64) {
                core::arch::asm!("dc cvac, {0}", in(reg) (self.queue_va + p), options(nomem, nostack));
                core::arch::asm!("dc cvac, {0}", in(reg) (req_va + p), options(nomem, nostack));
            }
            core::arch::asm!("dsb sy", options(nomem, nostack));
        }

        // Notify device: Queue 0
        self.write32(VIRTIO_REG_QUEUE_NOTIFY, 0);

        // Wait for device to finish processing in Used ring at queue_va + used_offset
        let used = (self.queue_va + used_offset as u64) as *const VirtqUsed<128>;
        let target_idx = cur_avail_idx.wrapping_add(1);

        let mut spins = 0u64;
        while spins < 50_000_000 {
            #[cfg(target_os = "uefi")]
            core::arch::asm!("dc ivac, {0}", in(reg) (self.queue_va + used_offset as u64), options(nomem, nostack));

            if core::ptr::read_volatile(&(*used).idx) == target_idx {
                break;
            }
            core::hint::spin_loop();
            spins += 1;
        }

        if spins >= 50_000_000 {
            return Err("VirtIO request timed out");
        }

        #[cfg(target_os = "uefi")]
        {
            for p in (0..1024).step_by(64) {
                core::arch::asm!("dc ivac, {0}", in(reg) (req_va + p), options(nomem, nostack));
            }
            core::arch::asm!("dsb sy", options(nomem, nostack));
        }

        let status = core::ptr::read_volatile((req_va + 0x300) as *const u8);
        if status != 0 {
            if status == 1 {
                return Err("VirtIO request returned VIRTIO_BLK_S_IOERR (1)");
            } else if status == 2 {
                return Err("VirtIO request returned VIRTIO_BLK_S_UNSUPP (2)");
            } else {
                return Err("VirtIO request returned unknown status");
            }
        }

        let b510 = core::ptr::read_volatile((req_va + 0x100 + 510) as *const u8);
        let b511 = core::ptr::read_volatile((req_va + 0x100 + 511) as *const u8);
        let sig = ((b510 as u16) << 8) | (b511 as u16);

        Ok(sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_blk_struct_sizes_and_alignments() {
        assert_eq!(core::mem::size_of::<VirtqDesc>(), 16);
        assert_eq!(core::mem::align_of::<VirtqDesc>(), 16);

        assert_eq!(core::mem::size_of::<VirtIOBlkReqHeader>(), 16);
        assert_eq!(core::mem::size_of::<VirtqUsedElem>(), 8);

        assert_eq!(VIRTIO_MAGIC, 0x7472_6976);
        assert_eq!(VIRTIO_DEV_BLOCK, 2);
    }
}

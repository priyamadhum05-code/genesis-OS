//! VirtIO Net MMIO Driver for Project Genesis (Stage 12B).
//!
//! Provides device probing across QEMU `virt` MMIO slots (0x0A00_0000..0x0A00_3E00),
//! 6-byte MAC address readout, legacy/modern queue initialization with 2-page alignment,
//! and synchronous Ethernet frame transmit / receive.

pub const VIRTIO_MMIO_BASE: u64 = 0x0A00_0000;
pub const VIRTIO_MMIO_STRIDE: u64 = 0x200;
pub const VIRTIO_MMIO_SLOTS: usize = 32;

pub const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"
pub const VIRTIO_DEV_NET: u32 = 1;

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
pub const VIRTIO_REG_CONFIG_SPACE: usize = 0x100;

pub const NET_QUEUE_SIZE: usize = 16;
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

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

pub struct VirtIONetDevice {
    pub base_va: u64,
    pub version: u32,
    pub mac: [u8; 6],
    pub rx_queue_pa: u64,
    pub rx_queue_va: u64,
    pub tx_queue_pa: u64,
    pub tx_queue_va: u64,
    pub rx_buf_pa: u64,
    pub rx_buf_va: u64,
    pub tx_buf_pa: u64,
    pub tx_buf_va: u64,
    pub queue_size: u16,
    pub rx_avail_idx: u16,
    pub rx_last_used_idx: u16,
    pub tx_avail_idx: u16,
    pub tx_last_used_idx: u16,
    pub packets_tx: u64,
    pub packets_rx: u64,
}

#[inline]
pub unsafe fn clean_dcache_range(va: u64, len: usize) {
    #[cfg(target_os = "uefi")]
    {
        let start = va & !63;
        let end = (va + len as u64 + 63) & !63;
        let mut p = start;
        while p < end {
            core::arch::asm!("dc cvac, {0}", in(reg) p, options(nomem, nostack));
            p += 64;
        }
        core::arch::asm!("dsb sy", options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    let _ = (va, len);
}

#[inline]
pub unsafe fn invalidate_dcache_range(va: u64, len: usize) {
    #[cfg(target_os = "uefi")]
    {
        let start = va & !63;
        let end = (va + len as u64 + 63) & !63;
        let mut p = start;
        while p < end {
            core::arch::asm!("dc ivac, {0}", in(reg) p, options(nomem, nostack));
            p += 64;
        }
        core::arch::asm!("dsb sy", options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    let _ = (va, len);
}

impl VirtIONetDevice {
    #[inline]
    unsafe fn read32(&self, offset: usize) -> u32 {
        core::ptr::read_volatile((self.base_va + offset as u64) as *const u32)
    }

    #[inline]
    unsafe fn write32(&self, offset: usize, val: u32) {
        core::ptr::write_volatile((self.base_va + offset as u64) as *mut u32, val)
    }

    /// Probes VirtIO MMIO slots for DeviceID == 1 (Network Device).
    /// Returns `Ok(Some(dev))` if found and initialized, `Ok(None)` if not found.
    pub unsafe fn probe(
        base_va: u64,
        rx_queue_pa: u64,
        rx_queue_va: u64,
        tx_queue_pa: u64,
        tx_queue_va: u64,
        rx_buf_pa: u64,
        rx_buf_va: u64,
        tx_buf_pa: u64,
        tx_buf_va: u64,
    ) -> Result<Option<Self>, &'static str> {
        let mut found_slot: Option<u64> = None;

        for slot in 0..VIRTIO_MMIO_SLOTS {
            let slot_va = base_va + (slot as u64) * VIRTIO_MMIO_STRIDE;
            let magic = core::ptr::read_volatile(slot_va as *const u32);
            if magic != VIRTIO_MAGIC {
                continue;
            }
            let dev_id = core::ptr::read_volatile((slot_va + VIRTIO_REG_DEVICE_ID as u64) as *const u32);
            if dev_id == VIRTIO_DEV_NET {
                found_slot = Some(slot_va);
                break;
            }
        }

        let slot_va = match found_slot {
            Some(va) => va,
            None => return Ok(None),
        };

        let mut dev = Self {
            base_va: slot_va,
            version: 0,
            mac: [0; 6],
            rx_queue_pa,
            rx_queue_va,
            tx_queue_pa,
            tx_queue_va,
            rx_buf_pa,
            rx_buf_va,
            tx_buf_pa,
            tx_buf_va,
            queue_size: NET_QUEUE_SIZE as u16,
            rx_avail_idx: 0,
            rx_last_used_idx: 0,
            tx_avail_idx: 0,
            tx_last_used_idx: 0,
            packets_tx: 0,
            packets_rx: 0,
        };

        let version = dev.read32(VIRTIO_REG_VERSION);
        dev.version = version;

        // Reset
        dev.write32(VIRTIO_REG_STATUS, 0);
        // ACKNOWLEDGE (1)
        dev.write32(VIRTIO_REG_STATUS, 1);
        // DRIVER (2)
        let status = dev.read32(VIRTIO_REG_STATUS);
        dev.write32(VIRTIO_REG_STATUS, status | 2);

        // Feature negotiation
        if version >= 2 {
            dev.write32(VIRTIO_REG_DEV_FEAT_SEL, 0);
            let _f0 = dev.read32(VIRTIO_REG_DEV_FEAT);
            dev.write32(VIRTIO_REG_DEV_FEAT_SEL, 1);
            let f1 = dev.read32(VIRTIO_REG_DEV_FEAT);

            dev.write32(VIRTIO_REG_DRV_FEAT_SEL, 0);
            dev.write32(VIRTIO_REG_DRV_FEAT, 0);
            dev.write32(VIRTIO_REG_DRV_FEAT_SEL, 1);
            dev.write32(VIRTIO_REG_DRV_FEAT, f1 & 1); // VIRTIO_F_VERSION_1

            // FEATURES_OK (8)
            let cur = dev.read32(VIRTIO_REG_STATUS);
            dev.write32(VIRTIO_REG_STATUS, cur | 8);
            if (dev.read32(VIRTIO_REG_STATUS) & 8) == 0 {
                return Err("VirtIO-Net features rejected by device");
            }
        }

        // Read MAC Address from config space (0x100..0x106)
        let mac_lo = dev.read32(VIRTIO_REG_CONFIG_SPACE);
        let mac_hi = dev.read32(VIRTIO_REG_CONFIG_SPACE + 4);
        dev.mac[0] = (mac_lo & 0xFF) as u8;
        dev.mac[1] = ((mac_lo >> 8) & 0xFF) as u8;
        dev.mac[2] = ((mac_lo >> 16) & 0xFF) as u8;
        dev.mac[3] = ((mac_lo >> 24) & 0xFF) as u8;
        dev.mac[4] = (mac_hi & 0xFF) as u8;
        dev.mac[5] = ((mac_hi >> 8) & 0xFF) as u8;

        // Zero queue memory (2 contiguous pages = 8192 bytes each)
        core::ptr::write_bytes(rx_queue_va as *mut u8, 0, 8192);
        core::ptr::write_bytes(tx_queue_va as *mut u8, 0, 8192);

        // Configure Queue 0 (RX)
        dev.write32(VIRTIO_REG_QUEUE_SEL, 0);
        let max_rx = dev.read32(VIRTIO_REG_QUEUE_NUM_MAX);
        if max_rx == 0 {
            return Err("VirtIO-Net RX queue not available");
        }
        dev.write32(VIRTIO_REG_QUEUE_NUM, NET_QUEUE_SIZE as u32);

        if version >= 2 {
            let desc_pa = rx_queue_pa;
            let avail_pa = rx_queue_pa + 256;
            let used_pa = rx_queue_pa + 4096; // 4096 alignment for VirtqUsed
            dev.write32(VIRTIO_REG_QUEUE_DESC_LO, (desc_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DESC_HI, (desc_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_LO, (avail_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_HI, (avail_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_LO, (used_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_HI, (used_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_READY, 1);
        } else {
            dev.write32(VIRTIO_REG_GUEST_PAGE_SIZE, 4096);
            dev.write32(VIRTIO_REG_QUEUE_ALIGN, 4096);
            dev.write32(VIRTIO_REG_QUEUE_PFN, (rx_queue_pa >> 12) as u32);
        }

        // Configure Queue 1 (TX)
        dev.write32(VIRTIO_REG_QUEUE_SEL, 1);
        let max_tx = dev.read32(VIRTIO_REG_QUEUE_NUM_MAX);
        if max_tx == 0 {
            return Err("VirtIO-Net TX queue not available");
        }
        dev.write32(VIRTIO_REG_QUEUE_NUM, NET_QUEUE_SIZE as u32);

        if version >= 2 {
            let desc_pa = tx_queue_pa;
            let avail_pa = tx_queue_pa + 256;
            let used_pa = tx_queue_pa + 4096;
            dev.write32(VIRTIO_REG_QUEUE_DESC_LO, (desc_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DESC_HI, (desc_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_LO, (avail_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_HI, (avail_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_LO, (used_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_HI, (used_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_READY, 1);
        } else {
            dev.write32(VIRTIO_REG_GUEST_PAGE_SIZE, 4096);
            dev.write32(VIRTIO_REG_QUEUE_ALIGN, 4096);
            dev.write32(VIRTIO_REG_QUEUE_PFN, (tx_queue_pa >> 12) as u32);
        }

        // Set DRIVER_OK (4)
        let cur = dev.read32(VIRTIO_REG_STATUS);
        dev.write32(VIRTIO_REG_STATUS, cur | 4);

        // Populate RX Descriptors and Avail Ring with receive buffers
        // Each buffer is 2048 bytes (allowing up to standard 1514 MTU frame + 10 byte header)
        let rx_descs = rx_queue_va as *mut VirtqDesc;
        let rx_avail = (rx_queue_va + 256) as *mut VirtqAvail<NET_QUEUE_SIZE>;
        for i in 0..NET_QUEUE_SIZE {
            let desc = rx_descs.add(i);
            (*desc).addr = rx_buf_pa + (i as u64) * 2048;
            (*desc).len = 1536;
            (*desc).flags = VIRTQ_DESC_F_WRITE;
            (*desc).next = 0;
            (*rx_avail).ring[i] = i as u16;
        }
        dev.rx_avail_idx = NET_QUEUE_SIZE as u16;
        (*rx_avail).flags = 0;
        (*rx_avail).idx = dev.rx_avail_idx;

        // Clean RX queue memory to PoC
        clean_dcache_range(rx_queue_va, 512);

        // Notify device that RX buffers are available on Queue 0
        dev.write32(VIRTIO_REG_QUEUE_NOTIFY, 0);

        Ok(Some(dev))
    }

    /// Transmits an Ethernet frame synchronously.
    pub unsafe fn send_packet(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        let total_len = 10 + frame.len();
        if total_len > 2048 {
            return Err("Packet exceeds TX buffer capacity");
        }

        // Write zeroed 10-byte VirtioNetHdr followed by Ethernet frame
        let tx_ptr = self.tx_buf_va as *mut u8;
        core::ptr::write_bytes(tx_ptr, 0, 10);
        core::ptr::copy_nonoverlapping(frame.as_ptr(), tx_ptr.add(10), frame.len());

        // Setup TX descriptor 0
        let tx_descs = self.tx_queue_va as *mut VirtqDesc;
        let desc0 = tx_descs.add(0);
        (*desc0).addr = self.tx_buf_pa;
        (*desc0).len = total_len as u32;
        (*desc0).flags = 0;
        (*desc0).next = 0;

        let tx_avail = (self.tx_queue_va + 256) as *mut VirtqAvail<NET_QUEUE_SIZE>;
        let ring_idx = (self.tx_avail_idx % NET_QUEUE_SIZE as u16) as usize;
        (*tx_avail).ring[ring_idx] = 0;
        self.tx_avail_idx = self.tx_avail_idx.wrapping_add(1);
        (*tx_avail).idx = self.tx_avail_idx;

        // Clean TX buffer and queue to PoC
        clean_dcache_range(self.tx_buf_va, total_len);
        clean_dcache_range(self.tx_queue_va, 512);

        // Notify device: Queue 1 (TX)
        self.write32(VIRTIO_REG_QUEUE_NOTIFY, 1);

        // Poll TX Used Ring
        let tx_used = (self.tx_queue_va + 4096) as *const VirtqUsed<NET_QUEUE_SIZE>;
        let mut spins = 0usize;
        loop {
            invalidate_dcache_range(tx_used as u64, 64);
            if (*tx_used).idx != self.tx_last_used_idx {
                self.tx_last_used_idx = (*tx_used).idx;
                break;
            }
            spins += 1;
            if spins > 2_000_000 {
                return Err("VirtIO-Net TX timeout");
            }
            core::hint::spin_loop();
        }

        self.packets_tx += 1;
        Ok(())
    }

    /// Polls for a received Ethernet frame.
    /// If a packet is available, returns `Some((buffer_slice, len))` and re-queues the descriptor.
    pub unsafe fn poll_packet<'a>(&mut self, out_buf: &'a mut [u8]) -> Option<usize> {
        let rx_used = (self.rx_queue_va + 4096) as *const VirtqUsed<NET_QUEUE_SIZE>;
        invalidate_dcache_range(rx_used as u64, 64);

        if (*rx_used).idx == self.rx_last_used_idx {
            return None;
        }

        let ring_idx = (self.rx_last_used_idx % NET_QUEUE_SIZE as u16) as usize;
        let elem = (*rx_used).ring[ring_idx];
        self.rx_last_used_idx = self.rx_last_used_idx.wrapping_add(1);

        let desc_id = (elem.id as usize) % NET_QUEUE_SIZE;
        let total_len = elem.len as usize;

        let buf_va = self.rx_buf_va + (desc_id as u64) * 2048;
        invalidate_dcache_range(buf_va, total_len);

        // Ethernet frame begins after 10-byte VirtioNetHdr (or 12-byte if modern with MRG_RXBUF)
        let (hdr_len, frame_len) = if total_len >= 12 && *((buf_va + 12) as *const u8) == 0x08 {
            (12, total_len - 12)
        } else if total_len >= 10 {
            (10, total_len - 10)
        } else {
            (0, 0)
        };

        let copy_len = frame_len.min(out_buf.len());
        if copy_len > 0 {
            core::ptr::copy_nonoverlapping(
                (buf_va + hdr_len as u64) as *const u8,
                out_buf.as_mut_ptr(),
                copy_len,
            );
        }

        // Re-queue descriptor into RX Avail Ring
        let rx_avail = (self.rx_queue_va + 256) as *mut VirtqAvail<NET_QUEUE_SIZE>;
        let avail_slot = (self.rx_avail_idx % NET_QUEUE_SIZE as u16) as usize;
        (*rx_avail).ring[avail_slot] = desc_id as u16;
        self.rx_avail_idx = self.rx_avail_idx.wrapping_add(1);
        (*rx_avail).idx = self.rx_avail_idx;

        clean_dcache_range(self.rx_queue_va, 512);
        self.write32(VIRTIO_REG_QUEUE_NOTIFY, 0);

        if copy_len > 0 {
            self.packets_rx += 1;
            Some(copy_len)
        } else {
            None
        }
    }

    /// Receives a packet (alias for poll_packet matching NetDevice trait).
    #[inline]
    pub unsafe fn recv_packet(&mut self, out: &mut [u8]) -> Option<usize> {
        self.poll_packet(out)
    }

    /// Prints MAC address in formatted `xx:xx:xx:xx:xx:xx` form.
    pub unsafe fn print_mac(&self, uart: usize) {
        for i in 0..6 {
            let b = self.mac[i];
            let hi = (b >> 4) & 0xF;
            let lo = b & 0xF;
            let c_hi = if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) };
            let c_lo = if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) };
            crate::arch::serial_write(uart, &[c_hi, c_lo]);
            if i < 5 {
                crate::arch::serial_write(uart, b":");
            }
        }
    }
}

pub static mut GLOBAL_VIRTIO_NET: Option<*mut VirtIONetDevice> = None;

pub fn register_global_virtio_net(dev: *mut VirtIONetDevice) {
    unsafe {
        *core::ptr::addr_of_mut!(GLOBAL_VIRTIO_NET) = Some(dev);
    }
}

pub fn get_global_virtio_net() -> Option<*mut VirtIONetDevice> {
    unsafe { *core::ptr::addr_of!(GLOBAL_VIRTIO_NET) }
}

pub fn get_global_virtio_net_ref() -> Option<&'static VirtIONetDevice> {
    unsafe {
        let ptr = *core::ptr::addr_of!(GLOBAL_VIRTIO_NET);
        ptr.map(|p| &*p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_net_struct_sizes_and_alignments() {
        assert_eq!(core::mem::size_of::<VirtqDesc>(), 16);
        assert_eq!(core::mem::align_of::<VirtqDesc>(), 16);
        assert_eq!(core::mem::size_of::<VirtioNetHdr>(), 10);
        assert_eq!(core::mem::size_of::<VirtqUsedElem>(), 8);
    }
}

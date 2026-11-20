//! PCIe ECAM Bus Enumeration and Configuration Access Mechanism.

pub const PCI_ECAM_DEFAULT_BASE: u64 = 0x40_1000_0000;
pub const PCI_ECAM_VA_BASE: u64 = 0xFFFF_FFFF_E100_0000;
pub const PCI_MMIO_32_BASE: u64 = 0x1000_0000;
pub const PCI_BAR_VA_BASE: u64 = 0xFFFF_FFFF_E200_0000;

pub const PCI_CAP_ID_VNDR: u8 = 0x09;

pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;
pub const VIRTIO_PCI_CAP_PCI_CFG: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PciCapability {
    pub cap_id: u8,
    pub cap_offset: u8,
    pub cap_len: u8,
    pub cfg_type: u8,
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    pub notify_off_multiplier: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDeviceInfo {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
    pub bars: [u64; 6],
    pub bar_sizes: [u64; 6],
    pub bar_is_io: [bool; 6],
    pub bar_is_64: [bool; 6],
    pub capabilities: [PciCapability; 8],
    pub cap_count: usize,
    pub has_msi: bool,
    pub msi_64bit: bool,
    pub has_msix: bool,
    pub msix_table_size: u16,
    pub msix_table_bar: u8,
    pub msix_table_offset: u32,
    pub msix_pba_bar: u8,
}

impl Default for PciDeviceInfo {
    fn default() -> Self {
        Self {
            bus: 0,
            dev: 0,
            func: 0,
            vendor_id: 0,
            device_id: 0,
            class_code: 0,
            subclass: 0,
            prog_if: 0,
            revision: 0,
            header_type: 0,
            bars: [0; 6],
            bar_sizes: [0; 6],
            bar_is_io: [false; 6],
            bar_is_64: [false; 6],
            capabilities: [PciCapability::default(); 8],
            cap_count: 0,
            has_msi: false,
            msi_64bit: false,
            has_msix: false,
            msix_table_size: 0,
            msix_table_bar: 0,
            msix_table_offset: 0,
            msix_pba_bar: 0,
        }
    }
}

pub static mut PCI_DEVICES: [PciDeviceInfo; 64] = [PciDeviceInfo {
    bus: 0,
    dev: 0,
    func: 0,
    vendor_id: 0,
    device_id: 0,
    class_code: 0,
    subclass: 0,
    prog_if: 0,
    revision: 0,
    header_type: 0,
    bars: [0; 6],
    bar_sizes: [0; 6],
    bar_is_io: [false; 6],
    bar_is_64: [false; 6],
    capabilities: [PciCapability {
        cap_id: 0,
        cap_offset: 0,
        cap_len: 0,
        cfg_type: 0,
        bar: 0,
        offset: 0,
        length: 0,
        notify_off_multiplier: 0,
    }; 8],
    cap_count: 0,
    has_msi: false,
    msi_64bit: false,
    has_msix: false,
    msix_table_size: 0,
    msix_table_bar: 0,
    msix_table_offset: 0,
    msix_pba_bar: 0,
}; 64];
pub static mut PCI_DEVICE_COUNT: usize = 0;

/// Returns ECAM byte offset for bus, dev, func, reg
pub const fn ecam_offset(bus: u8, dev: u8, func: u8, reg: u16) -> u64 {
    ((bus as u64) << 20) | ((dev as u64) << 15) | ((func as u64) << 12) | ((reg as u64) & 0xFFF)
}

/// Returns the configuration virtual/physical address in ECAM
pub const fn ecam_config_addr(ecam_base: u64, bus: u8, dev: u8, func: u8, reg: u16) -> u64 {
    ecam_base + ecam_offset(bus, dev, func, reg)
}

pub unsafe fn pci_read_u8(ecam_va: u64, bus: u8, dev: u8, func: u8, reg: u16) -> u8 {
    let addr = ecam_config_addr(ecam_va, bus, dev, func, reg) as *const u8;
    core::ptr::read_volatile(addr)
}

pub unsafe fn pci_read_u16(ecam_va: u64, bus: u8, dev: u8, func: u8, reg: u16) -> u16 {
    let addr = ecam_config_addr(ecam_va, bus, dev, func, reg) as *const u16;
    core::ptr::read_volatile(addr)
}

pub unsafe fn pci_read_u32(ecam_va: u64, bus: u8, dev: u8, func: u8, reg: u16) -> u32 {
    let addr = ecam_config_addr(ecam_va, bus, dev, func, reg) as *const u32;
    core::ptr::read_volatile(addr)
}

pub unsafe fn pci_write_u16(ecam_va: u64, bus: u8, dev: u8, func: u8, reg: u16, val: u16) {
    let addr = ecam_config_addr(ecam_va, bus, dev, func, reg) as *mut u16;
    core::ptr::write_volatile(addr, val);
}

pub unsafe fn pci_write_u32(ecam_va: u64, bus: u8, dev: u8, func: u8, reg: u16, val: u32) {
    let addr = ecam_config_addr(ecam_va, bus, dev, func, reg) as *mut u32;
    core::ptr::write_volatile(addr, val);
}

/// Recursively scan a PCI bus and downstream buses via bridges
pub unsafe fn scan_pci_bus(ecam_va: u64, bus: u8, max_bus: u8) {
    if bus > max_bus {
        return;
    }

    for dev in 0..32u8 {
        let mut max_func = 1u8;
        for func in 0..8u8 {
            if func >= max_func {
                break;
            }

            let vendor_id = pci_read_u16(ecam_va, bus, dev, func, 0x00);
            if vendor_id == 0xFFFF || vendor_id == 0x0000 {
                continue;
            }

            let device_id = pci_read_u16(ecam_va, bus, dev, func, 0x02);
            let revision = pci_read_u8(ecam_va, bus, dev, func, 0x08);
            let prog_if = pci_read_u8(ecam_va, bus, dev, func, 0x09);
            let subclass = pci_read_u8(ecam_va, bus, dev, func, 0x0A);
            let class_code = pci_read_u8(ecam_va, bus, dev, func, 0x0B);
            let header_type_raw = pci_read_u8(ecam_va, bus, dev, func, 0x0E);
            let header_type = header_type_raw & 0x7F;

            if func == 0 && (header_type_raw & 0x80) != 0 {
                max_func = 8;
            }

            let mut info = PciDeviceInfo {
                bus,
                dev,
                func,
                vendor_id,
                device_id,
                class_code,
                subclass,
                prog_if,
                revision,
                header_type,
                bars: [0; 6],
                bar_sizes: [0; 6],
                bar_is_io: [false; 6],
                bar_is_64: [false; 6],
                capabilities: [PciCapability::default(); 8],
                cap_count: 0,
                has_msi: false,
                msi_64bit: false,
                has_msix: false,
                msix_table_size: 0,
                msix_table_bar: 0,
                msix_table_offset: 0,
                msix_pba_bar: 0,
            };

            // Read BARs: Type 0 has 6 BARs, Type 1 (bridge) has 2 BARs
            let num_bars = if header_type == 1 { 2 } else { 6 };
            let mut b = 0usize;
            while b < num_bars {
                let reg_off = (0x10 + b * 4) as u16;
                let bar_val = pci_read_u32(ecam_va, bus, dev, func, reg_off);
                let is_io = (bar_val & 1) != 0;
                info.bar_is_io[b] = is_io;

                if is_io {
                    info.bars[b] = (bar_val & !0x3) as u64;
                    b += 1;
                    continue;
                }

                let is_64 = ((bar_val >> 1) & 0x3) == 0x2;
                info.bar_is_64[b] = is_64;

                // Probe BAR size by writing all 1s
                pci_write_u32(ecam_va, bus, dev, func, reg_off, 0xFFFF_FFFF);
                let mask = pci_read_u32(ecam_va, bus, dev, func, reg_off);
                pci_write_u32(ecam_va, bus, dev, func, reg_off, bar_val);

                let size_low = if mask != 0 && mask != 0xFFFF_FFFF {
                    (!(mask & !0xF)).wrapping_add(1) as u64
                } else {
                    0
                };

                if is_64 && b + 1 < num_bars {
                    let high_reg_off = (0x10 + (b + 1) * 4) as u16;
                    let bar_high = pci_read_u32(ecam_va, bus, dev, func, high_reg_off);

                    pci_write_u32(ecam_va, bus, dev, func, high_reg_off, 0xFFFF_FFFF);
                    let mask_high = pci_read_u32(ecam_va, bus, dev, func, high_reg_off);
                    pci_write_u32(ecam_va, bus, dev, func, high_reg_off, bar_high);

                    let mut addr = ((bar_high as u64) << 32) | ((bar_val & !0xF) as u64);
                    let total_size = if mask_high != 0 || size_low != 0 {
                        let combined_mask = ((mask_high as u64) << 32) | ((mask & !0xF) as u64);
                        (!combined_mask).wrapping_add(1)
                    } else {
                        0
                    };

                    // If unmapped by firmware, assign address from 32-bit MMIO pool
                    if addr == 0 && total_size > 0 {
                        let assigned = PCI_MMIO_32_BASE + (dev as u64 * 0x0100_0000) + (b as u64 * 0x0020_0000);
                        pci_write_u32(ecam_va, bus, dev, func, reg_off, assigned as u32);
                        pci_write_u32(ecam_va, bus, dev, func, high_reg_off, 0);
                        addr = assigned;
                    }

                    info.bars[b] = addr;
                    info.bar_sizes[b] = total_size;
                    b += 2;
                } else {
                    let mut addr = (bar_val & !0xF) as u64;
                    if addr == 0 && size_low > 0 {
                        let assigned = PCI_MMIO_32_BASE + (dev as u64 * 0x0100_0000) + (b as u64 * 0x0020_0000);
                        pci_write_u32(ecam_va, bus, dev, func, reg_off, assigned as u32);
                        addr = assigned;
                    }
                    info.bars[b] = addr;
                    info.bar_sizes[b] = size_low;
                    b += 1;
                }
            }

            // Enable Memory Space (bit 1) and Bus Master (bit 2)
            let cmd = pci_read_u16(ecam_va, bus, dev, func, 0x04);
            pci_write_u16(ecam_va, bus, dev, func, 0x04, cmd | 0x0006);

            // Scan capabilities list (offset 0x34) if status bit 4 is set
            let status = pci_read_u16(ecam_va, bus, dev, func, 0x06);
            if (status & (1 << 4)) != 0 {
                let mut cap_ptr = pci_read_u8(ecam_va, bus, dev, func, 0x34);
                let mut cap_count = 0;
                while cap_ptr != 0 && cap_count < 8 {
                    let cap_id = pci_read_u8(ecam_va, bus, dev, func, cap_ptr as u16);
                    let cap_next = pci_read_u8(ecam_va, bus, dev, func, (cap_ptr + 1) as u16);
                    let cap_len = pci_read_u8(ecam_va, bus, dev, func, (cap_ptr + 2) as u16);

                    let mut cap = PciCapability {
                        cap_id,
                        cap_offset: cap_ptr,
                        cap_len,
                        cfg_type: 0,
                        bar: 0,
                        offset: 0,
                        length: 0,
                        notify_off_multiplier: 0,
                    };

                    if cap_id == PCI_CAP_ID_VNDR && cap_len >= 16 {
                        cap.cfg_type = pci_read_u8(ecam_va, bus, dev, func, (cap_ptr + 3) as u16);
                        cap.bar = pci_read_u8(ecam_va, bus, dev, func, (cap_ptr + 4) as u16);
                        cap.offset = pci_read_u32(ecam_va, bus, dev, func, (cap_ptr + 8) as u16);
                        cap.length = pci_read_u32(ecam_va, bus, dev, func, (cap_ptr + 12) as u16);
                        if cap.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG && cap_len >= 20 {
                            cap.notify_off_multiplier = pci_read_u32(ecam_va, bus, dev, func, (cap_ptr + 16) as u16);
                        }
                    } else if cap_id == 0x05 {
                        let msg_ctrl = pci_read_u16(ecam_va, bus, dev, func, (cap_ptr + 2) as u16);
                        info.has_msi = true;
                        info.msi_64bit = (msg_ctrl & (1 << 7)) != 0;
                    } else if cap_id == 0x11 {
                        let msg_ctrl = pci_read_u16(ecam_va, bus, dev, func, (cap_ptr + 2) as u16);
                        let tbl = pci_read_u32(ecam_va, bus, dev, func, (cap_ptr + 4) as u16);
                        let pba = pci_read_u32(ecam_va, bus, dev, func, (cap_ptr + 8) as u16);
                        info.has_msix = true;
                        info.msix_table_size = (msg_ctrl & 0x7FF) + 1;
                        info.msix_table_bar = (tbl & 0x7) as u8;
                        info.msix_table_offset = tbl & !0x7;
                        info.msix_pba_bar = (pba & 0x7) as u8;
                    }

                    info.capabilities[cap_count] = cap;
                    cap_count += 1;
                    cap_ptr = cap_next;
                }
                info.cap_count = cap_count;
            }

            if PCI_DEVICE_COUNT < 64 {
                PCI_DEVICES[PCI_DEVICE_COUNT] = info;
                PCI_DEVICE_COUNT += 1;
            }

            // If PCI-to-PCI bridge, enable it and scan downstream buses
            if header_type == 1 {
                let sec_bus = pci_read_u8(ecam_va, bus, dev, func, 0x19);
                let sub_bus = pci_read_u8(ecam_va, bus, dev, func, 0x1A);
                // Also enable I/O, Memory, Bus Master on bridge
                let br_cmd = pci_read_u16(ecam_va, bus, dev, func, 0x04);
                pci_write_u16(ecam_va, bus, dev, func, 0x04, br_cmd | 0x0007);

                if sec_bus > bus && sec_bus <= max_bus && sub_bus >= sec_bus {
                    let end_bus = sub_bus.min(max_bus);
                    for downstream in sec_bus..=end_bus {
                        scan_pci_bus(ecam_va, downstream, max_bus);
                    }
                }
            }
        }
    }
}

/// Enumerate all devices across PCIe buses up to `max_bus`
pub unsafe fn scan_pci_buses(ecam_va: u64, max_bus: u8) -> usize {
    PCI_DEVICE_COUNT = 0;
    scan_pci_bus(ecam_va, 0, max_bus);
    PCI_DEVICE_COUNT
}

/// Enumerate all devices on PCIe Bus 0 (and downstream bridge buses up to 15)
pub unsafe fn scan_bus_0(ecam_va: u64) -> usize {
    scan_pci_buses(ecam_va, 15)
}

/// Find a device by Vendor ID and Device ID
pub unsafe fn find_pci_device(vendor_id: u16, device_id: u16) -> Option<PciDeviceInfo> {
    for i in 0..PCI_DEVICE_COUNT {
        if PCI_DEVICES[i].vendor_id == vendor_id && PCI_DEVICES[i].device_id == device_id {
            return Some(PCI_DEVICES[i]);
        }
    }
    None
}

/// Find a device by class_code, subclass, and prog_if
pub unsafe fn find_pci_by_class(class_code: u8, subclass: u8, prog_if: u8) -> Option<PciDeviceInfo> {
    for i in 0..PCI_DEVICE_COUNT {
        if PCI_DEVICES[i].class_code == class_code
            && PCI_DEVICES[i].subclass == subclass
            && PCI_DEVICES[i].prog_if == prog_if
        {
            return Some(PCI_DEVICES[i]);
        }
    }
    None
}

/// Enable PCI Memory Space (bit 1) and Bus Master (bit 2)
pub unsafe fn pci_enable_bus_master(ecam_va: u64, bus: u8, dev: u8, func: u8) {
    let cmd = pci_read_u16(ecam_va, bus, dev, func, 0x04);
    pci_write_u16(ecam_va, bus, dev, func, 0x04, cmd | 0x0006);
}

/// Return human-readable class description
pub fn pci_class_description(class_code: u8, subclass: u8) -> &'static str {
    match class_code {
        0x01 => match subclass {
            0x00 => "SCSI controller",
            0x01 => "IDE controller",
            0x06 => "SATA controller",
            0x08 => "NVMe controller",
            _ => "Mass storage controller",
        },
        0x02 => match subclass {
            0x00 => "Ethernet controller",
            _ => "Network controller",
        },
        0x03 => match subclass {
            0x00 => "VGA compatible controller",
            _ => "Display controller",
        },
        0x06 => match subclass {
            0x00 => "Host bridge",
            0x01 => "ISA bridge",
            0x04 => "PCI-to-PCI bridge",
            _ => "Bridge device",
        },
        0x0C => match subclass {
            0x03 => "USB controller",
            _ => "Serial bus controller",
        },
        _ => "Other device",
    }
}

/// Format PCI device line into buffer, returning &str
pub fn format_pci_device<'a>(dev: &PciDeviceInfo, buf: &'a mut [u8]) -> &'a str {
    fn hex4(val: u16, out: &mut [u8]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out[0] = HEX[((val >> 12) & 0xF) as usize];
        out[1] = HEX[((val >> 8) & 0xF) as usize];
        out[2] = HEX[((val >> 4) & 0xF) as usize];
        out[3] = HEX[(val & 0xF) as usize];
    }

    fn hex2(val: u8, out: &mut [u8]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out[0] = HEX[((val >> 4) & 0xF) as usize];
        out[1] = HEX[(val & 0xF) as usize];
    }

    fn hex8(val: u32, out: &mut [u8]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for i in 0..8 {
            out[7 - i] = HEX[((val >> (i * 4)) & 0xF) as usize];
        }
    }

    let mut p = 0;
    // Prefix: [00:02.0]
    buf[p] = b'['; p += 1;
    hex2(dev.bus, &mut buf[p..p + 2]); p += 2;
    buf[p] = b':'; p += 1;
    hex2(dev.dev, &mut buf[p..p + 2]); p += 2;
    buf[p] = b'.'; p += 1;
    buf[p] = b'0' + (dev.func & 0x7); p += 1;
    let mid1 = b"] ";
    buf[p..p + mid1.len()].copy_from_slice(mid1); p += mid1.len();

    // 1af4:1050
    hex4(dev.vendor_id, &mut buf[p..p + 4]); p += 4;
    buf[p] = b':'; p += 1;
    hex4(dev.device_id, &mut buf[p..p + 4]); p += 4;
    let mid2 = b" Class ";
    buf[p..p + mid2.len()].copy_from_slice(mid2); p += mid2.len();

    // Class code
    hex2(dev.class_code, &mut buf[p..p + 2]); p += 2;
    hex2(dev.subclass, &mut buf[p..p + 2]); p += 2;
    let mid3 = b" (";
    buf[p..p + mid3.len()].copy_from_slice(mid3); p += mid3.len();

    let desc = pci_class_description(dev.class_code, dev.subclass).as_bytes();
    let dlen = desc.len().min(buf.len() - p - 1);
    buf[p..p + dlen].copy_from_slice(&desc[..dlen]); p += dlen;
    buf[p] = b')'; p += 1;

    // First non-zero BAR
    for b in 0..6 {
        if dev.bars[b] != 0 {
            let bpre = b" BAR";
            buf[p..p + bpre.len()].copy_from_slice(bpre); p += bpre.len();
            buf[p] = b'0' + (b as u8); p += 1;
            let eq = b"=0x";
            buf[p..p + eq.len()].copy_from_slice(eq); p += eq.len();
            if dev.bars[b] > 0xFFFF_FFFF {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                for i in 0..16 {
                    buf[p + 15 - i] = HEX[((dev.bars[b] >> (i * 4)) & 0xF) as usize];
                }
                p += 16;
            } else {
                hex8(dev.bars[b] as u32, &mut buf[p..p + 8]);
                p += 8;
            }
            break;
        }
    }

    // MSI / MSI-X Capability tags
    if dev.has_msix {
        let tag = b" [MSI-X]";
        let tlen = tag.len().min(buf.len() - p);
        buf[p..p + tlen].copy_from_slice(&tag[..tlen]);
        p += tlen;
    } else if dev.has_msi {
        let tag = b" [MSI]";
        let tlen = tag.len().min(buf.len() - p);
        buf[p..p + tlen].copy_from_slice(&tag[..tlen]);
        p += tlen;
    }

    core::str::from_utf8(&buf[..p]).unwrap_or("[PCI Device]")
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec;
    use super::*;

    #[test]
    fn pci_ecam_address_math_test() {
        // Standard PCIe ECAM:
        // Bus 0, Dev 0, Func 0, Reg 0 -> offset 0
        assert_eq!(ecam_offset(0, 0, 0, 0), 0);
        assert_eq!(ecam_config_addr(0x40_1000_0000, 0, 0, 0, 0), 0x40_1000_0000);

        // Bus 0, Dev 2, Func 0, Reg 0 -> dev 2 = 2 << 15 = 0x10000
        assert_eq!(ecam_offset(0, 2, 0, 0), 0x10000);
        assert_eq!(ecam_config_addr(0x40_1000_0000, 0, 2, 0, 0), 0x40_1001_0000);

        // Bus 0, Dev 2, Func 0, Reg 0x10 -> BAR0
        assert_eq!(ecam_offset(0, 2, 0, 0x10), 0x10010);
        assert_eq!(ecam_config_addr(0x40_1000_0000, 0, 2, 0, 0x10), 0x40_1001_0010);

        // Bus 1, Dev 0, Func 0, Reg 0 -> bus 1 = 1 << 20 = 0x100000
        assert_eq!(ecam_offset(1, 0, 0, 0), 0x10_0000);
        assert_eq!(ecam_config_addr(0x40_1000_0000, 1, 0, 0, 0), 0x40_1010_0000);
    }

    #[test]
    fn pci_device_class_description_test() {
        assert_eq!(pci_class_description(0x03, 0x00), "VGA compatible controller");
        assert_eq!(pci_class_description(0x03, 0x80), "Display controller");
        assert_eq!(pci_class_description(0x02, 0x00), "Ethernet controller");
        assert_eq!(pci_class_description(0x06, 0x00), "Host bridge");
    }

    #[test]
    fn pci_format_device_string_test() {
        let dev = PciDeviceInfo {
            bus: 0,
            dev: 2,
            func: 0,
            vendor_id: 0x1AF4,
            device_id: 0x1050,
            class_code: 0x03,
            subclass: 0x00,
            prog_if: 0,
            revision: 1,
            header_type: 0,
            bars: [0x1000_0000, 0, 0, 0, 0, 0],
            bar_sizes: [0x4000, 0, 0, 0, 0, 0],
            bar_is_io: [false; 6],
            bar_is_64: [false; 6],
            capabilities: [PciCapability::default(); 8],
            cap_count: 0,
            has_msi: true,
            msi_64bit: true,
            has_msix: false,
            msix_table_size: 0,
            msix_table_bar: 0,
            msix_table_offset: 0,
            msix_pba_bar: 0,
        };

        let mut buf = [0u8; 128];
        let str_rep = format_pci_device(&dev, &mut buf);
        assert!(str_rep.contains("[00:02.0]"));
        assert!(str_rep.contains("1af4:1050"));
        assert!(str_rep.contains("Class 0300"));
        assert!(str_rep.contains("BAR0=0x10000000"));
        assert!(str_rep.contains("[MSI]"));
    }

    #[test]
    fn pci_bridge_secondary_bus_scanning_test() {
        // Allocate 2 MiB mock ECAM region (Bus 0 and Bus 1)
        let mut mock_ecam = vec![0u8; 2 * 1024 * 1024];
        let mock_va = mock_ecam.as_mut_ptr() as u64;

        // 1. Bus 0 Dev 1 Func 0: PCI-to-PCI Bridge (Type 1)
        let b0_d1_off = (0 << 20) | (1 << 15) | (0 << 12);
        mock_ecam[b0_d1_off..b0_d1_off + 2].copy_from_slice(&0x1B36u16.to_le_bytes()); // Vendor ID 0x1B36
        mock_ecam[b0_d1_off + 2..b0_d1_off + 4].copy_from_slice(&0x000Cu16.to_le_bytes()); // Device ID 0x000C
        mock_ecam[b0_d1_off + 8..b0_d1_off + 12].copy_from_slice(&[1, 0, 0x04, 0x06]); // Rev 1, Class 0604
        mock_ecam[b0_d1_off + 0x0E] = 1; // Header Type 1 (Bridge)
        mock_ecam[b0_d1_off + 0x18] = 0; // Primary bus 0
        mock_ecam[b0_d1_off + 0x19] = 1; // Secondary bus 1
        mock_ecam[b0_d1_off + 0x1A] = 1; // Subordinate bus 1

        // 2. Bus 1 Dev 0 Func 0: VirtIO-GPU device (Type 0)
        let b1_d0_off = (1 << 20) | (0 << 15) | (0 << 12);
        mock_ecam[b1_d0_off..b1_d0_off + 2].copy_from_slice(&0x1AF4u16.to_le_bytes()); // Vendor ID 0x1AF4
        mock_ecam[b1_d0_off + 2..b1_d0_off + 4].copy_from_slice(&0x1050u16.to_le_bytes()); // Device ID 0x1050
        mock_ecam[b1_d0_off + 8..b1_d0_off + 12].copy_from_slice(&[1, 0, 0x00, 0x03]); // Rev 1, Class 0300
        mock_ecam[b1_d0_off + 0x0E] = 0; // Header Type 0 (Endpoint)

        unsafe {
            let count = scan_pci_buses(mock_va, 1);
            assert_eq!(count, 2);
            assert_eq!(PCI_DEVICES[0].bus, 0);
            assert_eq!(PCI_DEVICES[0].dev, 1);
            assert_eq!(PCI_DEVICES[0].vendor_id, 0x1B36);
            assert_eq!(PCI_DEVICES[0].device_id, 0x000C);
            assert_eq!(PCI_DEVICES[0].header_type & 0x7F, 1);

            assert_eq!(PCI_DEVICES[1].bus, 1);
            assert_eq!(PCI_DEVICES[1].dev, 0);
            assert_eq!(PCI_DEVICES[1].vendor_id, 0x1AF4);
            assert_eq!(PCI_DEVICES[1].device_id, 0x1050);
            assert_eq!(PCI_DEVICES[1].header_type & 0x7F, 0);
        }
    }
}

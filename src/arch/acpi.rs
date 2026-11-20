//! Project Genesis — ACPI 2.0+ RSDP/XSDT Table Parser for AArch64 (Stage 21).
//!
//! Provides validation and extraction of hardware metadata from ACPI tables:
//! - RSDP (Root System Description Pointer v2)
//! - XSDT (Extended System Description Table)
//! - MADT (Multiple APIC Description Table / GIC Distributor & CPUs)
//! - MCFG (PCIe ECAM Configuration Space Base Address)
//! - GTDT (Generic Timer Description Table / ARM PPI GSIVs)
//! - SPCR (Serial Port Console Redirection Table / PL011 UART)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AcpiHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AcpiRsdp {
    pub revision: u8,
    pub length: u32,
    pub xsdt_pa: u64,
    pub oem_id: [u8; 6],
    pub rsdp_pa: u64,
}

impl AcpiRsdp {
    pub fn oem_id_str(&self) -> &str {
        core::str::from_utf8(&self.oem_id).unwrap_or("??????")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcpiMadtInfo {
    pub gicd_base: u64,
    pub gicr_base: u64,
    pub gicr_size: u64,
    pub cpu_count: usize,
    pub enabled_cpus: usize,
    pub cpu_mpidrs: [u64; 16],
}

impl Default for AcpiMadtInfo {
    fn default() -> Self {
        Self {
            gicd_base: 0,
            gicr_base: 0,
            gicr_size: 0,
            cpu_count: 0,
            enabled_cpus: 0,
            cpu_mpidrs: [0; 16],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AcpiMcfgEntry {
    pub ecam_base: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AcpiGtdtInfo {
    pub phys_el1_timer_gsiv: u32,
    pub virt_el1_timer_gsiv: u32,
    pub phys_el2_timer_gsiv: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AcpiSpcrInfo {
    pub interface_type: u8,
    pub uart_base: u64,
    pub baud_rate: u8,
    pub gsiv: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct AcpiSummary {
    pub rsdp: AcpiRsdp,
    pub xsdt_entries_count: usize,
    pub madt: Option<AcpiMadtInfo>,
    pub mcfg: Option<AcpiMcfgEntry>,
    pub gtdt: Option<AcpiGtdtInfo>,
    pub spcr: Option<AcpiSpcrInfo>,
    pub is_valid: bool,
}

/// Compute 8-bit checksum over a byte slice. Valid ACPI tables sum to 0 mod 256.
pub fn validate_checksum(slice: &[u8]) -> bool {
    let sum = slice.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    sum == 0
}

/// Parse and validate ACPI 2.0+ Root System Description Pointer (RSDP).
pub fn parse_rsdp(slice: &[u8]) -> Option<AcpiRsdp> {
    if slice.len() < 36 {
        return None;
    }
    if &slice[0..8] != b"RSD PTR " {
        return None;
    }
    // Checksum of first 20 bytes (ACPI 1.0 prefix)
    if !validate_checksum(&slice[0..20]) {
        return None;
    }

    let mut oem_id = [0u8; 6];
    oem_id.copy_from_slice(&slice[9..15]);

    let revision = slice[15];
    if revision >= 2 {
        let length = u32::from_le_bytes([slice[20], slice[21], slice[22], slice[23]]);
        if (length as usize) < 36 || slice.len() < length as usize {
            return None;
        }
        // Extended checksum over entire structure
        if !validate_checksum(&slice[0..length as usize]) {
            return None;
        }
        let xsdt_pa = u64::from_le_bytes([
            slice[24], slice[25], slice[26], slice[27],
            slice[28], slice[29], slice[30], slice[31],
        ]);
        Some(AcpiRsdp {
            revision,
            length,
            xsdt_pa,
            oem_id,
            rsdp_pa: 0,
        })
    } else {
        Some(AcpiRsdp {
            revision,
            length: 20,
            xsdt_pa: 0,
            oem_id,
            rsdp_pa: 0,
        })
    }
}

/// Parse and validate standard ACPI SDT 36-byte header.
pub fn parse_sdt_header(slice: &[u8]) -> Option<AcpiHeader> {
    if slice.len() < 36 {
        return None;
    }
    let length = u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]);
    if (length as usize) < 36 || slice.len() < length as usize {
        return None;
    }
    if !validate_checksum(&slice[0..length as usize]) {
        return None;
    }

    let mut sig = [0u8; 4];
    sig.copy_from_slice(&slice[0..4]);
    let revision = slice[8];
    let checksum = slice[9];
    let mut oem_id = [0u8; 6];
    oem_id.copy_from_slice(&slice[10..16]);
    let mut oem_table_id = [0u8; 8];
    oem_table_id.copy_from_slice(&slice[16..24]);
    let oem_revision = u32::from_le_bytes([slice[24], slice[25], slice[26], slice[27]]);
    let creator_id = u32::from_le_bytes([slice[28], slice[29], slice[30], slice[31]]);
    let creator_revision = u32::from_le_bytes([slice[32], slice[33], slice[34], slice[35]]);

    Some(AcpiHeader {
        signature: sig,
        length,
        revision,
        checksum,
        oem_id,
        oem_table_id,
        oem_revision,
        creator_id,
        creator_revision,
    })
}

/// Parse Multiple APIC Description Table ("APIC") for ARM GIC architecture.
pub fn parse_madt(slice: &[u8]) -> Option<AcpiMadtInfo> {
    if slice.len() < 44 || &slice[0..4] != b"APIC" {
        return None;
    }
    let length = u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]) as usize;
    if slice.len() < length || length < 44 {
        return None;
    }
    if !validate_checksum(&slice[0..length]) {
        return None;
    }

    let mut info = AcpiMadtInfo::default();
    let mut offset = 44; // Skip SDT header (36) + Local APIC address (4) + Flags (4)

    while offset + 2 <= length {
        let st_type = slice[offset];
        let st_len = slice[offset + 1] as usize;
        if st_len < 2 || offset + st_len > length {
            break;
        }

        match st_type {
            // Type 0x0B (11): GICC (GIC CPU Interface)
            0x0B => {
                if st_len >= 76 {
                    let flags = u32::from_le_bytes([
                        slice[offset + 12], slice[offset + 13], slice[offset + 14], slice[offset + 15],
                    ]);
                    let mpidr = u64::from_le_bytes([
                        slice[offset + 68], slice[offset + 69], slice[offset + 70], slice[offset + 71],
                        slice[offset + 72], slice[offset + 73], slice[offset + 74], slice[offset + 75],
                    ]);
                    if (flags & 1) != 0 {
                        if info.enabled_cpus < 16 {
                            info.cpu_mpidrs[info.enabled_cpus] = mpidr;
                        }
                        info.enabled_cpus += 1;
                    }
                    info.cpu_count += 1;
                }
            }
            // Type 0x0C (12): GICD (GIC Distributor)
            0x0C => {
                if st_len >= 24 {
                    info.gicd_base = u64::from_le_bytes([
                        slice[offset + 8], slice[offset + 9], slice[offset + 10], slice[offset + 11],
                        slice[offset + 12], slice[offset + 13], slice[offset + 14], slice[offset + 15],
                    ]);
                }
            }
            // Type 0x0E (14): GICR (GIC Redistributor)
            0x0E => {
                if st_len >= 16 {
                    info.gicr_base = u64::from_le_bytes([
                        slice[offset + 4], slice[offset + 5], slice[offset + 6], slice[offset + 7],
                        slice[offset + 8], slice[offset + 9], slice[offset + 10], slice[offset + 11],
                    ]);
                    info.gicr_size = u32::from_le_bytes([
                        slice[offset + 12], slice[offset + 13], slice[offset + 14], slice[offset + 15],
                    ]) as u64;
                }
            }
            _ => {}
        }
        offset += st_len;
    }

    Some(info)
}

/// Parse MCFG table for PCIe ECAM base address.
pub fn parse_mcfg(slice: &[u8]) -> Option<AcpiMcfgEntry> {
    if slice.len() < 44 || &slice[0..4] != b"MCFG" {
        return None;
    }
    let length = u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]) as usize;
    if slice.len() < length || length < 44 {
        return None;
    }
    if !validate_checksum(&slice[0..length]) {
        return None;
    }

    // Offset 44 has first allocation structure (16 bytes)
    if length >= 60 {
        let ecam_base = u64::from_le_bytes([
            slice[44], slice[45], slice[46], slice[47],
            slice[48], slice[49], slice[50], slice[51],
        ]);
        let segment_group = u16::from_le_bytes([slice[52], slice[53]]);
        let start_bus = slice[54];
        let end_bus = slice[55];
        Some(AcpiMcfgEntry {
            ecam_base,
            segment_group,
            start_bus,
            end_bus,
        })
    } else {
        None
    }
}

/// Parse GTDT table for ARM Generic Timer PPI IRQs.
pub fn parse_gtdt(slice: &[u8]) -> Option<AcpiGtdtInfo> {
    if slice.len() < 76 || &slice[0..4] != b"GTDT" {
        return None;
    }
    let length = u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]) as usize;
    if slice.len() < length || length < 76 {
        return None;
    }
    if !validate_checksum(&slice[0..length]) {
        return None;
    }

    let phys_el1_timer_gsiv = u32::from_le_bytes([slice[56], slice[57], slice[58], slice[59]]);
    let virt_el1_timer_gsiv = u32::from_le_bytes([slice[64], slice[65], slice[66], slice[67]]);
    let phys_el2_timer_gsiv = u32::from_le_bytes([slice[72], slice[73], slice[74], slice[75]]);

    Some(AcpiGtdtInfo {
        phys_el1_timer_gsiv,
        virt_el1_timer_gsiv,
        phys_el2_timer_gsiv,
    })
}

/// Parse SPCR table for Serial Port Console Redirection (PL011 UART).
pub fn parse_spcr(slice: &[u8]) -> Option<AcpiSpcrInfo> {
    if slice.len() < 60 || &slice[0..4] != b"SPCR" {
        return None;
    }
    let length = u32::from_le_bytes([slice[4], slice[5], slice[6], slice[7]]) as usize;
    if slice.len() < length || length < 60 {
        return None;
    }
    if !validate_checksum(&slice[0..length]) {
        return None;
    }

    let interface_type = slice[36];
    // Offset 40..52: Generic Address Structure (GAS), offset 44..52 is 64-bit Address
    let uart_base = u64::from_le_bytes([
        slice[44], slice[45], slice[46], slice[47],
        slice[48], slice[49], slice[50], slice[51],
    ]);
    let gsiv = u32::from_le_bytes([slice[54], slice[55], slice[56], slice[57]]);
    let baud_rate = slice[58];

    Some(AcpiSpcrInfo {
        interface_type,
        uart_base,
        baud_rate,
        gsiv,
    })
}

/// Global ACPI Summary
pub static mut GLOBAL_ACPI: Option<AcpiSummary> = None;

pub fn register_global_acpi(summary: AcpiSummary) {
    unsafe {
        GLOBAL_ACPI = Some(summary);
    }
}

#[allow(static_mut_refs)]
pub fn get_global_acpi() -> Option<&'static AcpiSummary> {
    unsafe { GLOBAL_ACPI.as_ref() }
}

#[allow(static_mut_refs)]
pub fn get_global_acpi_ref() -> Option<&'static AcpiSummary> {
    unsafe { GLOBAL_ACPI.as_ref() }
}

/// In-kernel entry point: walk RSDP -> XSDT -> Tables using HHDM_BASE
pub unsafe fn parse_acpi_system(rsdp_pa: u64, hhdm_base: u64) -> Option<AcpiSummary> {
    if rsdp_pa == 0 {
        return None;
    }

    let rsdp_ptr = (hhdm_base + rsdp_pa) as *const u8;
    let rsdp_slice = core::slice::from_raw_parts(rsdp_ptr, 36);
    let mut rsdp = parse_rsdp(rsdp_slice)?;
    rsdp.rsdp_pa = rsdp_pa;

    if rsdp.xsdt_pa == 0 {
        return Some(AcpiSummary {
            rsdp,
            xsdt_entries_count: 0,
            madt: None,
            mcfg: None,
            gtdt: None,
            spcr: None,
            is_valid: true,
        });
    }

    let xsdt_ptr = (hhdm_base + rsdp.xsdt_pa) as *const u8;
    let xsdt_hdr_slice = core::slice::from_raw_parts(xsdt_ptr, 36);
    let xsdt_len = u32::from_le_bytes([xsdt_hdr_slice[4], xsdt_hdr_slice[5], xsdt_hdr_slice[6], xsdt_hdr_slice[7]]) as usize;
    if xsdt_len < 36 {
        return None;
    }

    let xsdt_slice = core::slice::from_raw_parts(xsdt_ptr, xsdt_len);
    if !validate_checksum(xsdt_slice) {
        return None;
    }

    let entry_count = (xsdt_len - 36) / 8;
    let mut summary = AcpiSummary {
        rsdp,
        xsdt_entries_count: entry_count,
        madt: None,
        mcfg: None,
        gtdt: None,
        spcr: None,
        is_valid: true,
    };
    let entries_ptr = xsdt_ptr.add(36) as *const u64;

    for i in 0..entry_count {
        let table_pa = core::ptr::read_unaligned(entries_ptr.add(i));
        if table_pa == 0 {
            continue;
        }

        let tbl_ptr = (hhdm_base + table_pa) as *const u8;
        let hdr_slice = core::slice::from_raw_parts(tbl_ptr, 36);
        let tbl_len = u32::from_le_bytes([hdr_slice[4], hdr_slice[5], hdr_slice[6], hdr_slice[7]]) as usize;
        if tbl_len < 36 {
            continue;
        }

        let full_slice = core::slice::from_raw_parts(tbl_ptr, tbl_len);
        let sig = &full_slice[0..4];

        if sig == b"APIC" {
            summary.madt = parse_madt(full_slice);
        } else if sig == b"MCFG" {
            summary.mcfg = parse_mcfg(full_slice);
        } else if sig == b"GTDT" {
            summary.gtdt = parse_gtdt(full_slice);
        } else if sig == b"SPCR" {
            summary.spcr = parse_spcr(full_slice);
        }
    }

    register_global_acpi(summary);
    Some(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_acpi_checksum(slice: &mut [u8], checksum_idx: usize) {
        slice[checksum_idx] = 0;
        let sum = slice.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        slice[checksum_idx] = (0u8).wrapping_sub(sum);
    }

    #[test]
    fn validate_checksum_test() {
        let mut buf = [1u8, 2, 3, 4, 0];
        compute_acpi_checksum(&mut buf, 4);
        assert!(validate_checksum(&buf));

        // Corrupt one byte
        buf[0] ^= 0xFF;
        assert!(!validate_checksum(&buf));
    }

    #[test]
    fn parse_rsdp_test() {
        let mut rsdp = [0u8; 36];
        rsdp[0..8].copy_from_slice(b"RSD PTR ");
        rsdp[9..15].copy_from_slice(b"BOCHS ");
        rsdp[15] = 2; // ACPI 2.0
        rsdp[16..20].copy_from_slice(&0x4000_0000u32.to_le_bytes()); // RSDT PA
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes()); // Length
        rsdp[24..32].copy_from_slice(&0x8000_1000u64.to_le_bytes()); // XSDT PA

        // Compute 20-byte first checksum
        compute_acpi_checksum(&mut rsdp[0..20], 8);
        // Compute 36-byte extended checksum
        compute_acpi_checksum(&mut rsdp, 32);

        let parsed = parse_rsdp(&rsdp).expect("parse_rsdp should succeed");
        assert_eq!(parsed.revision, 2);
        assert_eq!(parsed.length, 36);
        assert_eq!(parsed.xsdt_pa, 0x8000_1000);
        assert_eq!(parsed.oem_id_str(), "BOCHS ");

        // Reject if bad signature
        rsdp[0] = b'X';
        assert!(parse_rsdp(&rsdp).is_none());
    }

    #[test]
    fn parse_madt_test() {
        // Build mock MADT with 1 GICD (type 0x0C, len 24), 1 GICR (type 0x0E, len 16), and 2 GICCs (type 0x0B, len 80)
        let total_len = 44 + 24 + 16 + 80 + 80;
        let mut madt = [0u8; 244];
        madt[0..4].copy_from_slice(b"APIC");
        madt[4..8].copy_from_slice(&(total_len as u32).to_le_bytes());
        madt[8] = 3; // Revision
        madt[10..16].copy_from_slice(b"GENESI");

        // Offset 44: GICD
        let gicd_off = 44;
        madt[gicd_off] = 0x0C;
        madt[gicd_off + 1] = 24;
        madt[gicd_off + 8..gicd_off + 16].copy_from_slice(&0x0800_0000u64.to_le_bytes());

        // Offset 68: GICR
        let gicr_off = 68;
        madt[gicr_off] = 0x0E;
        madt[gicr_off + 1] = 16;
        madt[gicr_off + 4..gicr_off + 12].copy_from_slice(&0x080A_0000u64.to_le_bytes());
        madt[gicr_off + 12..gicr_off + 16].copy_from_slice(&0x000C_0000u32.to_le_bytes());

        // Offset 84: GICC CPU 0 (Enabled)
        let gicc0_off = 84;
        madt[gicc0_off] = 0x0B;
        madt[gicc0_off + 1] = 80;
        madt[gicc0_off + 12..gicc0_off + 16].copy_from_slice(&1u32.to_le_bytes()); // Enabled = 1
        madt[gicc0_off + 68..gicc0_off + 76].copy_from_slice(&0x00u64.to_le_bytes()); // MPIDR = 0

        // Offset 164: GICC CPU 1 (Enabled)
        let gicc1_off = 164;
        madt[gicc1_off] = 0x0B;
        madt[gicc1_off + 1] = 80;
        madt[gicc1_off + 12..gicc1_off + 16].copy_from_slice(&1u32.to_le_bytes()); // Enabled = 1
        madt[gicc1_off + 68..gicc1_off + 76].copy_from_slice(&0x01u64.to_le_bytes()); // MPIDR = 1

        compute_acpi_checksum(&mut madt[0..total_len], 9);

        let parsed = parse_madt(&madt[0..total_len]).expect("parse_madt should succeed");
        assert_eq!(parsed.gicd_base, 0x0800_0000);
        assert_eq!(parsed.gicr_base, 0x080A_0000);
        assert_eq!(parsed.gicr_size, 0x000C_0000);
        assert_eq!(parsed.cpu_count, 2);
        assert_eq!(parsed.enabled_cpus, 2);
        assert_eq!(parsed.cpu_mpidrs[0], 0x00);
        assert_eq!(parsed.cpu_mpidrs[1], 0x01);
    }

    #[test]
    fn parse_mcfg_test() {
        let total_len = 60;
        let mut mcfg = [0u8; 60];
        mcfg[0..4].copy_from_slice(b"MCFG");
        mcfg[4..8].copy_from_slice(&(total_len as u32).to_le_bytes());
        mcfg[8] = 1;
        mcfg[10..16].copy_from_slice(b"GENESI");

        // First allocation at offset 44
        mcfg[44..52].copy_from_slice(&0x40_1000_0000u64.to_le_bytes()); // ECAM base
        mcfg[52..54].copy_from_slice(&0u16.to_le_bytes()); // Segment group 0
        mcfg[54] = 0;  // Start bus
        mcfg[55] = 15; // End bus

        compute_acpi_checksum(&mut mcfg, 9);

        let parsed = parse_mcfg(&mcfg).expect("parse_mcfg should succeed");
        assert_eq!(parsed.ecam_base, 0x40_1000_0000);
        assert_eq!(parsed.segment_group, 0);
        assert_eq!(parsed.start_bus, 0);
        assert_eq!(parsed.end_bus, 15);
    }

    #[test]
    fn parse_gtdt_test() {
        let total_len = 76;
        let mut gtdt = [0u8; 76];
        gtdt[0..4].copy_from_slice(b"GTDT");
        gtdt[4..8].copy_from_slice(&(total_len as u32).to_le_bytes());
        gtdt[8] = 2;
        gtdt[10..16].copy_from_slice(b"GENESI");

        // PhysEL1 GSIV at 56
        gtdt[56..60].copy_from_slice(&30u32.to_le_bytes());
        // VirtEL1 GSIV at 64
        gtdt[64..68].copy_from_slice(&27u32.to_le_bytes());
        // PhysEL2 GSIV at 72
        gtdt[72..76].copy_from_slice(&26u32.to_le_bytes());

        compute_acpi_checksum(&mut gtdt, 9);

        let parsed = parse_gtdt(&gtdt).expect("parse_gtdt should succeed");
        assert_eq!(parsed.phys_el1_timer_gsiv, 30);
        assert_eq!(parsed.virt_el1_timer_gsiv, 27);
        assert_eq!(parsed.phys_el2_timer_gsiv, 26);
    }

    #[test]
    fn parse_spcr_test() {
        let total_len = 60;
        let mut spcr = [0u8; 60];
        spcr[0..4].copy_from_slice(b"SPCR");
        spcr[4..8].copy_from_slice(&(total_len as u32).to_le_bytes());
        spcr[8] = 2;
        spcr[10..16].copy_from_slice(b"GENESI");

        spcr[36] = 3; // Interface type: ARM PL011
        // Base address at 44..52
        spcr[44..52].copy_from_slice(&0x0900_0000u64.to_le_bytes());
        // GSIV at 54..58
        spcr[54..58].copy_from_slice(&33u32.to_le_bytes());
        // Baud rate at 58
        spcr[58] = 7; // 115200

        compute_acpi_checksum(&mut spcr, 9);

        let parsed = parse_spcr(&spcr).expect("parse_spcr should succeed");
        assert_eq!(parsed.interface_type, 3);
        assert_eq!(parsed.uart_base, 0x0900_0000);
        assert_eq!(parsed.gsiv, 33);
        assert_eq!(parsed.baud_rate, 7);
    }
}

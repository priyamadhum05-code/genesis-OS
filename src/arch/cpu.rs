//! AArch64 CPU Identification, Feature Detection & Hardware Telemetry (Stage 20).
//!
//! Decodes MIDR_EL1 (CPU model/part), MPIDR_EL1 (affinity), ID_AA64ISAR0_EL1 (ISA features),
//! ID_AA64PFR0_EL1 (FP/NEON), CNTFRQ_EL0 / CNTVCT_EL0 (timer frequency & uptime),
//! and online SMP core statistics.

use core::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTelemetry {
    pub implementer: u8,
    pub variant: u8,
    pub architecture: u8,
    pub part_num: u16,
    pub revision: u8,
    pub online_cores: u64,
    pub has_fp: bool,
    pub has_neon: bool,
    pub has_aes: bool,
    pub has_sha1: bool,
    pub has_sha2: bool,
    pub has_crc32: bool,
    pub has_lse: bool,
    pub has_dotprod: bool,
    pub timer_freq_hz: u64,
    pub timer_ticks: u64,
    pub uptime_seconds: u64,
    pub irq_count: u64,
}

#[inline]
pub fn read_midr() -> u64 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {0}, midr_el1", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(target_os = "uefi"))]
    0x410FD0B0 // Cortex-A76 r0p0
}

#[inline]
pub fn read_mpidr() -> u64 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {0}, mpidr_el1", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(target_os = "uefi"))]
    0x8000_0000
}

#[inline]
pub fn read_isar0() -> u64 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {0}, id_aa64isar0_el1", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(target_os = "uefi"))]
    0x0001_0112_1122 // AES, SHA1, SHA2, CRC32, ATOMICS, DP
}

#[inline]
pub fn read_pfr0() -> u64 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {0}, id_aa64pfr0_el1", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(target_os = "uefi"))]
    0x0000_0000 // FP=0, AdvSIMD=0 (implemented)
}

#[inline]
pub fn read_cntfrq() -> u64 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(target_os = "uefi"))]
    62_500_000 // 62.5 MHz typical QEMU timer freq
}

#[inline]
pub fn read_cntvct() -> u64 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let val: u64;
        core::arch::asm!("mrs {0}, cntvct_el0", out(reg) val, options(nomem, nostack));
        val
    }
    #[cfg(not(target_os = "uefi"))]
    125_000_000 // 2 seconds
}

/// Query and decode full CPU telemetry and system statistics.
pub fn query_cpu_telemetry() -> CpuTelemetry {
    let midr = read_midr();
    let isar0 = read_isar0();
    let pfr0 = read_pfr0();
    let cntfrq = read_cntfrq();
    let cntvct = read_cntvct();

    let implementer = ((midr >> 24) & 0xFF) as u8;
    let variant = ((midr >> 20) & 0xF) as u8;
    let architecture = ((midr >> 16) & 0xF) as u8;
    let part_num = ((midr >> 4) & 0xFFF) as u16;
    let revision = (midr & 0xF) as u8;

    let has_aes = ((isar0 >> 4) & 0xF) >= 1;
    let has_sha1 = ((isar0 >> 8) & 0xF) >= 1;
    let has_sha2 = ((isar0 >> 12) & 0xF) >= 1;
    let has_crc32 = ((isar0 >> 16) & 0xF) >= 1;
    let has_lse = ((isar0 >> 20) & 0xF) >= 2;
    let has_dotprod = ((isar0 >> 44) & 0xF) >= 1;

    let has_fp = ((pfr0 >> 16) & 0xF) == 0;
    let has_neon = ((pfr0 >> 20) & 0xF) == 0;

    let online_cores = crate::arch::psci::ONLINE_CPUS.load(Ordering::Relaxed);
    let uptime_seconds = if cntfrq > 0 { cntvct / cntfrq } else { 0 };
    let irq_count = crate::arch::stage4_get_ticks();

    CpuTelemetry {
        implementer,
        variant,
        architecture,
        part_num,
        revision,
        online_cores,
        has_fp,
        has_neon,
        has_aes,
        has_sha1,
        has_sha2,
        has_crc32,
        has_lse,
        has_dotprod,
        timer_freq_hz: cntfrq,
        timer_ticks: cntvct,
        uptime_seconds,
        irq_count,
    }
}

pub fn part_num_to_str(part_num: u16) -> &'static str {
    match part_num {
        0xD03 => "Cortex-A53",
        0xD07 => "Cortex-A57",
        0xD08 => "Cortex-A72",
        0xD09 => "Cortex-A73",
        0xD0A => "Cortex-A75",
        0xD0B => "Cortex-A76",
        0xD0C => "Neoverse-N1",
        0xD40 => "Neoverse-V1",
        0xD41 => "Cortex-A78",
        0xD49 => "Neoverse-N2",
        _ => "ARMv8-A Compatible",
    }
}

pub fn implementer_to_str(imp: u8) -> &'static str {
    match imp {
        0x41 => "ARM Limited",
        0x43 => "Cavium",
        0x48 => "HiSilicon",
        0x51 => "Qualcomm",
        0x61 => "Apple",
        _ => "Generic AArch64",
    }
}

/// Format a comprehensive CPU telemetry summary into a buffer.
pub fn format_cpu_summary<'a>(t: &CpuTelemetry, buf: &'a mut [u8]) -> &'a str {
    fn write_num(mut val: u64, out: &mut [u8]) -> usize {
        if val == 0 {
            out[0] = b'0';
            return 1;
        }
        let mut tmp = [0u8; 20];
        let mut len = 0;
        while val > 0 {
            tmp[len] = b'0' + (val % 10) as u8;
            val /= 10;
            len += 1;
        }
        for i in 0..len {
            out[i] = tmp[len - 1 - i];
        }
        len
    }

    let mut p = 0;

    let s1 = b"CPU Model:      ";
    buf[p..p + s1.len()].copy_from_slice(s1); p += s1.len();
    let model = part_num_to_str(t.part_num).as_bytes();
    buf[p..p + model.len()].copy_from_slice(model); p += model.len();

    let s2 = b" (";
    buf[p..p + s2.len()].copy_from_slice(s2); p += s2.len();
    let imp = implementer_to_str(t.implementer).as_bytes();
    buf[p..p + imp.len()].copy_from_slice(imp); p += imp.len();
    let s3 = b", r";
    buf[p..p + s3.len()].copy_from_slice(s3); p += s3.len();
    p += write_num(t.variant as u64, &mut buf[p..]);
    let s4 = b"p";
    buf[p..p + s4.len()].copy_from_slice(s4); p += s4.len();
    p += write_num(t.revision as u64, &mut buf[p..]);
    buf[p] = b')'; p += 1;
    buf[p] = b'\n'; p += 1;

    let s5 = b"SMP Topology:   ";
    buf[p..p + s5.len()].copy_from_slice(s5); p += s5.len();
    p += write_num(t.online_cores, &mut buf[p..]);
    let s6 = b" Cores Online (AArch64 EL1)\n";
    buf[p..p + s6.len()].copy_from_slice(s6); p += s6.len();

    let s7 = b"ISA Features:   ";
    buf[p..p + s7.len()].copy_from_slice(s7); p += s7.len();
    let mut first = true;
    let mut add_feat = |name: &[u8], buf: &mut [u8], p: &mut usize| {
        if !first {
            buf[*p] = b','; *p += 1;
            buf[*p] = b' '; *p += 1;
        }
        first = false;
        buf[*p..*p + name.len()].copy_from_slice(name);
        *p += name.len();
    };

    if t.has_neon { add_feat(b"NEON", buf, &mut p); }
    if t.has_fp { add_feat(b"FP", buf, &mut p); }
    if t.has_aes { add_feat(b"AES", buf, &mut p); }
    if t.has_sha1 { add_feat(b"SHA1", buf, &mut p); }
    if t.has_sha2 { add_feat(b"SHA2", buf, &mut p); }
    if t.has_crc32 { add_feat(b"CRC32", buf, &mut p); }
    if t.has_lse { add_feat(b"LSE", buf, &mut p); }
    if t.has_dotprod { add_feat(b"DotProd", buf, &mut p); }
    buf[p] = b'\n'; p += 1;

    let s8 = b"Timer Clock:    ";
    buf[p..p + s8.len()].copy_from_slice(s8); p += s8.len();
    let mhz = t.timer_freq_hz / 1_000_000;
    p += write_num(mhz, &mut buf[p..]);
    let s9 = b" MHz | Uptime: ";
    buf[p..p + s9.len()].copy_from_slice(s9); p += s9.len();
    p += write_num(t.uptime_seconds, &mut buf[p..]);
    let s10 = b"s | IRQ Ticks: ";
    buf[p..p + s10.len()].copy_from_slice(s10); p += s10.len();
    p += write_num(t.irq_count, &mut buf[p..]);

    core::str::from_utf8(&buf[..p]).unwrap_or("[CPU Telemetry]")
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn cpu_midr_decoding_test() {
        let midr: u64 = 0x410FD0B2; // ARM Cortex-A76 r0p2
        let implementer = ((midr >> 24) & 0xFF) as u8;
        let variant = ((midr >> 20) & 0xF) as u8;
        let part_num = ((midr >> 4) & 0xFFF) as u16;
        let revision = (midr & 0xF) as u8;

        assert_eq!(implementer, 0x41);
        assert_eq!(implementer_to_str(implementer), "ARM Limited");
        assert_eq!(variant, 0);
        assert_eq!(part_num, 0xD0B);
        assert_eq!(part_num_to_str(part_num), "Cortex-A76");
        assert_eq!(revision, 2);
    }

    #[test]
    fn cpu_telemetry_query_and_format_test() {
        let t = query_cpu_telemetry();
        assert_eq!(t.part_num, 0xD0B);
        assert!(t.has_neon);
        assert!(t.has_aes);
        assert!(t.has_crc32);

        let mut buf = [0u8; 512];
        let summary = format_cpu_summary(&t, &mut buf);
        assert!(summary.contains("Cortex-A76"));
        assert!(summary.contains("ARM Limited"));
        assert!(summary.contains("NEON"));
        assert!(summary.contains("AES"));
        assert!(summary.contains("CRC32"));
        assert!(summary.contains("Cores Online"));
    }
}

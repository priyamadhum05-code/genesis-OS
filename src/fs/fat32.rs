//! Project Genesis — Read-Only FAT32 / FAT16 Filesystem Driver.
//!
//! Parses MBR partition tables, FAT BPB (Boot Parameter Block),
//! directory entries, and cluster chains to load arbitrary files into memory.

use crate::drivers::VirtIOBlockDevice;

/// Searches Sector 0 for a direct FAT BPB or an MBR partition table.
/// Returns the starting LBA sector of the FAT volume.
pub fn locate_volume(sector0: &[u8; 512]) -> Result<u64, &'static str> {
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Err("Invalid boot signature (expected 0x55AA)");
    }

    // Check if Sector 0 is directly a FAT BPB (superfloppy format)
    let bps = u16::from_le_bytes([sector0[11], sector0[12]]);
    let spc = sector0[13];
    if (sector0[0] == 0xEB || sector0[0] == 0xE9) && bps == 512 && spc > 0 && spc.is_power_of_two() {
        return Ok(0);
    }

    // Otherwise, parse MBR partition table at offsets 446, 462, 478, 494
    for i in 0..4 {
        let off = 446 + i * 16;
        let p_type = sector0[off + 4];
        let lba_start = u32::from_le_bytes([
            sector0[off + 8],
            sector0[off + 9],
            sector0[off + 10],
            sector0[off + 11],
        ]) as u64;
        let sec_count = u32::from_le_bytes([
            sector0[off + 12],
            sector0[off + 13],
            sector0[off + 14],
            sector0[off + 15],
        ]);

        if lba_start != 0 && sec_count != 0 {
            // Valid partition entry (e.g. FAT32 0x0B/0x0C, FAT16 0x06/0x0E, EFI 0xEF)
            let _ = p_type;
            return Ok(lba_start);
        }
    }

    Err("No valid FAT volume or MBR partition found in sector 0")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FatVolume {
    pub volume_lba: u64,
    pub bytes_per_sec: u16,
    pub sec_per_clus: u8,
    pub rsvd_sec_cnt: u16,
    pub num_fats: u8,
    pub fat_size: u32,
    pub root_clus: u32,
    pub fat_start_lba: u64,
    pub data_start_lba: u64,
    pub is_fat32: bool,
    pub root_dir_lba: u64,
    pub root_dir_sectors: u32,
}

impl FatVolume {
    /// Parses a 512-byte Volume Boot Record (BPB) at `volume_lba`.
    pub fn parse_bpb(volume_lba: u64, bpb: &[u8; 512]) -> Result<Self, &'static str> {
        if bpb[510] != 0x55 || bpb[511] != 0xAA {
            return Err("Invalid BPB boot signature");
        }

        let bytes_per_sec = u16::from_le_bytes([bpb[11], bpb[12]]);
        if bytes_per_sec != 512 {
            return Err("Unsupported sector size (only 512-byte sectors supported)");
        }

        let sec_per_clus = bpb[13];
        if sec_per_clus == 0 || !sec_per_clus.is_power_of_two() {
            return Err("Invalid sectors per cluster");
        }

        let rsvd_sec_cnt = u16::from_le_bytes([bpb[14], bpb[15]]);
        if rsvd_sec_cnt == 0 {
            return Err("Invalid reserved sector count");
        }

        let num_fats = bpb[16];
        if num_fats == 0 {
            return Err("Invalid number of FATs");
        }

        let root_ent_cnt = u16::from_le_bytes([bpb[17], bpb[18]]);
        let fat_sz_16 = u16::from_le_bytes([bpb[22], bpb[23]]);
        let fat_sz_32 = u32::from_le_bytes([bpb[36], bpb[37], bpb[38], bpb[39]]);

        let is_fat32 = root_ent_cnt == 0 && fat_sz_16 == 0;
        let fat_start_lba = volume_lba + rsvd_sec_cnt as u64;

        if is_fat32 {
            if fat_sz_32 == 0 {
                return Err("Invalid FAT32 table size");
            }
            let fat_size = fat_sz_32;
            let root_clus = u32::from_le_bytes([bpb[44], bpb[45], bpb[46], bpb[47]]);
            let data_start_lba = fat_start_lba + (num_fats as u64) * (fat_size as u64);

            Ok(Self {
                volume_lba,
                bytes_per_sec,
                sec_per_clus,
                rsvd_sec_cnt,
                num_fats,
                fat_size,
                root_clus,
                fat_start_lba,
                data_start_lba,
                is_fat32: true,
                root_dir_lba: 0,
                root_dir_sectors: 0,
            })
        } else {
            if fat_sz_16 == 0 {
                return Err("Invalid FAT16 table size");
            }
            let fat_size = fat_sz_16 as u32;
            let root_dir_sectors =
                ((root_ent_cnt as u32 * 32) + (bytes_per_sec as u32 - 1)) / (bytes_per_sec as u32);
            let root_dir_lba = fat_start_lba + (num_fats as u64) * (fat_size as u64);
            let data_start_lba = root_dir_lba + root_dir_sectors as u64;

            Ok(Self {
                volume_lba,
                bytes_per_sec,
                sec_per_clus,
                rsvd_sec_cnt,
                num_fats,
                fat_size,
                root_clus: 0,
                fat_start_lba,
                data_start_lba,
                is_fat32: false,
                root_dir_lba,
                root_dir_sectors,
            })
        }
    }

    /// Converts a cluster number (>= 2) to its starting LBA sector number.
    #[inline]
    pub fn cluster_to_lba(&self, cluster: u32) -> u64 {
        self.data_start_lba + ((cluster.saturating_sub(2)) as u64) * (self.sec_per_clus as u64)
    }

    /// Reads a file identified by `path` (e.g. `"WEIGHTS.BIN"` or `"EFI/BOOT/BOOTAA64.EFI"`)
    /// into caller-provided buffer `dest_pa` / `dest_va` up to `dest_cap` bytes.
    ///
    /// Uses `scratch_pa` / `scratch_va` (a 4 KiB scratch buffer) for reading directory and FAT sectors.
    /// Returns the number of bytes read.
    pub unsafe fn read_file(
        &self,
        dev: &mut VirtIOBlockDevice,
        path: &str,
        dest_pa: u64,
        dest_va: u64,
        dest_cap: usize,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<usize, &'static str> {
        let mut curr_dir_clus = if self.is_fat32 { self.root_clus } else { 0 };

        // Split path into slash-separated components
        let mut path_str = path;
        while path_str.starts_with('/') {
            path_str = &path_str[1..];
        }

        let mut components = ["" ; 8];
        let mut comp_count = 0;
        for part in path_str.split('/') {
            if !part.is_empty() {
                if comp_count >= components.len() {
                    return Err("Path depth exceeds maximum supported (8)");
                }
                components[comp_count] = part;
                comp_count += 1;
            }
        }

        if comp_count == 0 {
            return Err("Empty path specified");
        }

        let mut target_clus = 0u32;
        let mut file_size = 0usize;

        for (idx, comp) in components[..comp_count].iter().enumerate() {
            let target_8_3 = to_8_3(comp);
            let is_last = idx == comp_count - 1;

            let (entry_clus, entry_size, is_dir) = self.find_entry_in_dir(
                dev,
                curr_dir_clus,
                &target_8_3,
                scratch_pa,
                scratch_va,
            )?;

            if is_last {
                if is_dir {
                    return Err("Specified path is a directory, not a file");
                }
                target_clus = entry_clus;
                file_size = entry_size;
            } else {
                if !is_dir {
                    return Err("Intermediate path component is not a directory");
                }
                curr_dir_clus = entry_clus;
            }
        }

        if file_size > dest_cap {
            return Err("File size exceeds destination buffer capacity");
        }
        if file_size == 0 {
            return Ok(0);
        }

        // Read file contents by following FAT cluster chain
        let mut bytes_left = file_size;
        let mut curr_clus = target_clus;
        let mut dest_offset = 0usize;
        let clus_bytes = (self.sec_per_clus as usize) * (self.bytes_per_sec as usize);

        while bytes_left > 0 && curr_clus >= 2 {
            let clus_lba = self.cluster_to_lba(curr_clus);
            let sectors_to_read = self.sec_per_clus as u16;

            // Read cluster sectors into scratch buffer, then copy to destination
            // (or directly if cluster fits into remaining capacity)
            if dest_offset + sectors_to_read as usize * 512 <= dest_cap {
                dev.read_sectors(
                    clus_lba,
                    sectors_to_read,
                    dest_pa + dest_offset as u64,
                    dest_va + dest_offset as u64,
                )?;
                let read_len = bytes_left.min(clus_bytes);
                bytes_left -= read_len;
                dest_offset += read_len;
            } else {
                dev.read_sectors(clus_lba, sectors_to_read, scratch_pa, scratch_va)?;
                let read_len = bytes_left.min(clus_bytes);
                core::ptr::copy_nonoverlapping(
                    scratch_va as *const u8,
                    (dest_va + dest_offset as u64) as *mut u8,
                    read_len,
                );
                bytes_left -= read_len;
                dest_offset += read_len;
            }

            if bytes_left == 0 {
                break;
            }

            // Lookup next cluster in FAT
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
            if curr_clus >= 0x0FFF_FFF8 || (self.is_fat32 && curr_clus >= 0x0FFF_FFF7) {
                break;
            }
        }

        Ok(file_size)
    }

    /// Looks up the next cluster in the FAT for cluster `curr_clus`.
    unsafe fn get_next_cluster(
        &self,
        dev: &mut VirtIOBlockDevice,
        curr_clus: u32,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<u32, &'static str> {
        let entry_size = if self.is_fat32 { 4u64 } else { 2u64 };
        let fat_byte_off = (curr_clus as u64) * entry_size;
        let fat_sec = self.fat_start_lba + fat_byte_off / (self.bytes_per_sec as u64);
        let ent_off = (fat_byte_off % (self.bytes_per_sec as u64)) as usize;

        dev.read_sectors(fat_sec, 1, scratch_pa, scratch_va)?;

        if self.is_fat32 {
            let p = (scratch_va + ent_off as u64) as *const u8;
            let val = u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]) & 0x0FFF_FFFF;
            Ok(val)
        } else {
            let p = (scratch_va + ent_off as u64) as *const u8;
            let val = u16::from_le_bytes([*p, *p.add(1)]) as u32;
            Ok(val)
        }
    }

    /// Searches directory at `dir_clus` for entry matching `target_8_3`.
    /// Returns `(first_cluster, file_size, is_dir)`.
    unsafe fn find_entry_in_dir(
        &self,
        dev: &mut VirtIOBlockDevice,
        dir_clus: u32,
        target_8_3: &[u8; 11],
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(u32, usize, bool), &'static str> {
        if !self.is_fat32 && dir_clus == 0 {
            // FAT16 Root Directory
            for s in 0..self.root_dir_sectors {
                let sec = self.root_dir_lba + s as u64;
                dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
                if let Some(res) = self.scan_dir_sector(scratch_va, target_8_3) {
                    return Ok(res);
                }
            }
            return Err("File not found in FAT16 root directory");
        }

        // Cluster-based directory (FAT32 root or any subdirectory)
        let mut curr_clus = dir_clus;
        while curr_clus >= 2 && curr_clus < 0x0FFF_FFF8 {
            let start_lba = self.cluster_to_lba(curr_clus);
            for s in 0..self.sec_per_clus as u64 {
                dev.read_sectors(start_lba + s, 1, scratch_pa, scratch_va)?;
                if let Some(res) = self.scan_dir_sector(scratch_va, target_8_3) {
                    return Ok(res);
                }
            }

            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
        }

        Err("File not found in directory")
    }

    /// Scans a 512-byte directory sector at `scratch_va` for `target_8_3`.
    unsafe fn scan_dir_sector(
        &self,
        scratch_va: u64,
        target_8_3: &[u8; 11],
    ) -> Option<(u32, usize, bool)> {
        for e in 0..16 {
            let entry_ptr = (scratch_va + e * 32) as *const u8;
            let first_byte = *entry_ptr;
            if first_byte == 0x00 {
                // End of directory entries
                return None;
            }
            if first_byte == 0xE5 {
                // Unused/deleted entry
                continue;
            }

            let attr = *entry_ptr.add(11);
            if attr == 0x0F || (attr & 0x08) != 0 {
                // Long File Name or Volume ID
                continue;
            }

            let name_slice = core::slice::from_raw_parts(entry_ptr, 11);
            if name_slice.eq_ignore_ascii_case(target_8_3) {
                let is_dir = (attr & 0x10) != 0;
                let clus_hi = if self.is_fat32 {
                    u16::from_le_bytes([*entry_ptr.add(20), *entry_ptr.add(21)]) as u32
                } else {
                    0
                };
                let clus_lo = u16::from_le_bytes([*entry_ptr.add(26), *entry_ptr.add(27)]) as u32;
                let first_clus = (clus_hi << 16) | clus_lo;

                let size = u32::from_le_bytes([
                    *entry_ptr.add(28),
                    *entry_ptr.add(29),
                    *entry_ptr.add(30),
                    *entry_ptr.add(31),
                ]) as usize;

                return Some((first_clus, size, is_dir));
            }
        }
        None
    }
}

/// Formats a filename into standard 8.3 space-padded uppercase representation.
/// E.g. `"WEIGHTS.BIN"` -> `b"WEIGHTS BIN"`, `"EFI"` -> `b"EFI        "`.
pub fn to_8_3(name: &str) -> [u8; 11] {
    let mut res = [b' '; 11];
    let mut parts = name.splitn(2, '.');
    let base = parts.next().unwrap_or("");
    let ext = parts.next().unwrap_or("");

    let mut b_idx = 0;
    for ch in base.bytes() {
        if b_idx < 8 {
            res[b_idx] = ch.to_ascii_uppercase();
            b_idx += 1;
        }
    }

    let mut e_idx = 8;
    for ch in ext.bytes() {
        if e_idx < 11 {
            res[e_idx] = ch.to_ascii_uppercase();
            e_idx += 1;
        }
    }

    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_8_3_test() {
        assert_eq!(&to_8_3("WEIGHTS.BIN"), b"WEIGHTS BIN");
        assert_eq!(&to_8_3("weights.bin"), b"WEIGHTS BIN");
        assert_eq!(&to_8_3("BOOTAA64.EFI"), b"BOOTAA64EFI");
        assert_eq!(&to_8_3("EFI"), b"EFI        ");
        assert_eq!(&to_8_3("a.b"), b"A       B  ");
    }

    #[test]
    fn locate_volume_direct_and_mbr() {
        // 1. Direct FAT32 BPB
        let mut bpb = [0u8; 512];
        bpb[0] = 0xEB;
        bpb[1] = 0x58;
        bpb[2] = 0x90;
        bpb[11] = 0x00;
        bpb[12] = 0x02; // 512 bytes per sec
        bpb[13] = 8;    // 8 sec per clus
        bpb[510] = 0x55;
        bpb[511] = 0xAA;
        assert_eq!(locate_volume(&bpb), Ok(0));

        // 2. MBR with partition at LBA 2048
        let mut mbr = [0u8; 512];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        // Partition 1 at offset 446
        mbr[446 + 4] = 0x0C; // FAT32 LBA
        mbr[446 + 8] = 0x00;
        mbr[446 + 9] = 0x08; // LBA 2048 (0x0800)
        mbr[446 + 12] = 0x00;
        mbr[446 + 13] = 0x10; // 4096 sectors
        assert_eq!(locate_volume(&mbr), Ok(2048));
    }

    #[test]
    fn parse_fat32_bpb_fields() {
        let mut bpb = [0u8; 512];
        bpb[11] = 0x00;
        bpb[12] = 0x02; // 512
        bpb[13] = 4;    // 4 sec per clus
        bpb[14] = 32;   // 32 reserved sectors
        bpb[16] = 2;    // 2 FATs
        bpb[17] = 0;    // root_ent_cnt = 0 for FAT32
        bpb[22] = 0;    // fat_sz_16 = 0
        bpb[36] = 0x00;
        bpb[37] = 0x04; // fat_sz_32 = 1024 sectors
        bpb[44] = 2;    // root_clus = 2
        bpb[510] = 0x55;
        bpb[511] = 0xAA;

        let vol = FatVolume::parse_bpb(2048, &bpb).expect("valid FAT32 BPB");
        assert!(vol.is_fat32);
        assert_eq!(vol.bytes_per_sec, 512);
        assert_eq!(vol.sec_per_clus, 4);
        assert_eq!(vol.fat_start_lba, 2048 + 32);
        assert_eq!(vol.data_start_lba, 2048 + 32 + 2 * 1024);
        assert_eq!(vol.root_clus, 2);
        assert_eq!(vol.cluster_to_lba(2), vol.data_start_lba);
        assert_eq!(vol.cluster_to_lba(3), vol.data_start_lba + 4);
    }
}

//! Project Genesis — Read-Only FAT32 / FAT16 Filesystem Driver.
//!
//! Parses MBR partition tables, FAT BPB (Boot Parameter Block),
//! directory entries, and cluster chains to load arbitrary files into memory.

extern crate alloc;
use crate::fs::BlockDevice;

/// Computes the Microsoft FAT LFN checksum over an 11-byte short name.
pub fn lfn_checksum(short_name: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &b in short_name.iter() {
        sum = (((sum & 1) << 7) | (sum >> 1)).wrapping_add(b);
    }
    sum
}

/// Extracts 13 UCS-2 characters from a 32-byte VFAT LFN directory entry.
pub fn extract_lfn_chars(entry_bytes: &[u8; 32], out: &mut [u16; 13]) {
    for i in 0..5 {
        out[i] = u16::from_le_bytes([entry_bytes[1 + i * 2], entry_bytes[2 + i * 2]]);
    }
    for i in 0..6 {
        out[5 + i] = u16::from_le_bytes([entry_bytes[14 + i * 2], entry_bytes[15 + i * 2]]);
    }
    for i in 0..2 {
        out[11 + i] = u16::from_le_bytes([entry_bytes[28 + i * 2], entry_bytes[29 + i * 2]]);
    }
}

/// Constructs a 32-byte VFAT LFN directory entry.
pub fn build_lfn_entry(seq: u8, is_last: bool, checksum: u8, chars: &[u16]) -> [u8; 32] {
    let mut entry = [0u8; 32];
    entry[0] = (seq & 0x1F) | if is_last { 0x40 } else { 0 };
    entry[11] = 0x0F; // Attributes (READ_ONLY | HIDDEN | SYSTEM | VOLUME_ID)
    entry[12] = 0x00; // Type
    entry[13] = checksum;
    entry[26] = 0x00; // Cluster lo
    entry[27] = 0x00;

    let mut padded = [0xFFFFu16; 13];
    let copy_len = chars.len().min(13);
    for i in 0..copy_len {
        padded[i] = chars[i];
    }
    if copy_len < 13 {
        padded[copy_len] = 0x0000;
    }

    // Name 1: 5 chars (bytes 1..10)
    for i in 0..5 {
        let b = padded[i].to_le_bytes();
        entry[1 + i * 2] = b[0];
        entry[2 + i * 2] = b[1];
    }
    // Name 2: 6 chars (bytes 14..25)
    for i in 0..6 {
        let b = padded[5 + i].to_le_bytes();
        entry[14 + i * 2] = b[0];
        entry[15 + i * 2] = b[1];
    }
    // Name 3: 2 chars (bytes 28..31)
    for i in 0..2 {
        let b = padded[11 + i].to_le_bytes();
        entry[28 + i * 2] = b[0];
        entry[29 + i * 2] = b[1];
    }

    entry
}

/// Tracks and reconstructs Long File Names across directory slots.
#[derive(Clone, Copy)]
pub struct LfnAccumulator {
    pub chars: [u16; 64],
    pub char_len: usize,
    pub checksum: u8,
    pub valid: bool,
    pub slot_count: usize,
}

impl LfnAccumulator {
    pub const fn new() -> Self {
        Self {
            chars: [0; 64],
            char_len: 0,
            checksum: 0,
            valid: false,
            slot_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.char_len = 0;
        self.checksum = 0;
        self.valid = false;
        self.slot_count = 0;
    }

    pub fn process_lfn_slot(&mut self, entry_bytes: &[u8; 32]) {
        let seq_byte = entry_bytes[0];
        let is_last = (seq_byte & 0x40) != 0;
        let seq = (seq_byte & 0x1F) as usize;
        if seq == 0 || seq > 20 {
            self.reset();
            return;
        }

        let chk = entry_bytes[13];
        if is_last {
            self.reset();
            self.checksum = chk;
            self.valid = true;
        } else if !self.valid || self.checksum != chk {
            self.reset();
            return;
        }

        self.slot_count += 1;
        let mut slice = [0u16; 13];
        extract_lfn_chars(entry_bytes, &mut slice);

        let base_idx = (seq - 1) * 13;
        for i in 0..13 {
            let ch = slice[i];
            if ch == 0x0000 || ch == 0xFFFF {
                if base_idx + i < self.chars.len() {
                    self.char_len = self.char_len.max(base_idx + i);
                }
                break;
            }
            if base_idx + i < self.chars.len() {
                self.chars[base_idx + i] = ch;
                self.char_len = self.char_len.max(base_idx + i + 1);
            }
        }
    }

    pub fn to_utf8(&self, out: &mut [u8; 64]) -> usize {
        let mut p = 0;
        for i in 0..self.char_len.min(63) {
            let ch = self.chars[i];
            if ch == 0 || ch == 0xFFFF {
                break;
            }
            if ch < 0x80 {
                out[p] = ch as u8;
                p += 1;
            } else if ch < 0x800 && p + 1 < 64 {
                out[p] = (0xC0 | (ch >> 6)) as u8;
                out[p + 1] = (0x80 | (ch & 0x3F)) as u8;
                p += 2;
            } else if p < 64 {
                out[p] = b'?';
                p += 1;
            }
        }
        p
    }
}

/// Details of a located directory entry on disk.
#[derive(Clone, Copy, Debug)]
pub struct DirEntryLocation {
    pub sec: u64,
    pub slot_off: usize,
    pub lfn_count: usize,
    pub first_clus: u32,
    pub size: usize,
    pub is_dir: bool,
    pub short_name: [u8; 11],
}

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
        dev: &mut impl BlockDevice,
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
            let is_last = idx == comp_count - 1;

            let (entry_clus, entry_size, is_dir) = self.find_entry_in_dir(
                dev,
                curr_dir_clus,
                comp,
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

            if dest_offset + clus_bytes <= dest_cap {
                dev.read_sectors(
                    clus_lba,
                    sectors_to_read,
                    dest_pa + dest_offset as u64,
                    dest_va + dest_offset as u64,
                )?;
                if bytes_left <= clus_bytes {
                    break;
                }
                bytes_left -= clus_bytes;
                dest_offset += clus_bytes;
            } else {
                let mut clus_bytes_left = bytes_left.min(clus_bytes);
                let mut sec_off = 0u64;
                while clus_bytes_left > 0 {
                    let chunk = clus_bytes_left.min(4096);
                    let secs = ((chunk + 511) / 512) as u16;
                    dev.read_sectors(clus_lba + sec_off, secs, scratch_pa, scratch_va)?;
                    core::ptr::copy_nonoverlapping(
                        scratch_va as *const u8,
                        (dest_va + dest_offset as u64) as *mut u8,
                        chunk,
                    );
                    clus_bytes_left -= chunk;
                    dest_offset += chunk;
                    sec_off += secs as u64;
                }
                break;
            }

            if bytes_left == 0 {
                break;
            }

            // Lookup next cluster in FAT
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
            let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
            if curr_clus >= eof {
                break;
            }
        }

        Ok(file_size)
    }

    /// Reads up to `dest_cap` bytes from a file identified by `path`.
    /// Unlike `read_file`, does not fail if `file_size > dest_cap`;
    /// instead reads up to `dest_cap` bytes and returns the number of bytes read.
    pub unsafe fn read_file_partial(
        &self,
        dev: &mut impl BlockDevice,
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

        let mut components = [""; 8];
        let mut comp_count = 0;
        for part in path_str.split('/') {
            if !part.is_empty() {
                if comp_count >= components.len() {
                    return Err("Path exceeds maximum depth of 8 components");
                }
                components[comp_count] = part;
                comp_count += 1;
            }
        }
        if comp_count == 0 {
            return Err("Empty file path");
        }

        let mut target_clus = 0u32;
        let mut file_size = 0usize;

        for (idx, &comp) in components[..comp_count].iter().enumerate() {
            let is_last = idx == comp_count - 1;
            let (entry_clus, size, is_dir) =
                self.find_entry_in_dir(dev, curr_dir_clus, comp, scratch_pa, scratch_va)?;

            if is_last {
                if is_dir {
                    return Err("Path references a directory, not a file");
                }
                target_clus = entry_clus;
                file_size = size;
            } else {
                if !is_dir {
                    return Err("Intermediate path component is not a directory");
                }
                curr_dir_clus = entry_clus;
            }
        }

        let to_read = file_size.min(dest_cap);
        if to_read == 0 {
            return Ok(0);
        }

        // Read file contents by following FAT cluster chain
        let mut bytes_left = to_read;
        let mut curr_clus = target_clus;
        let mut dest_offset = 0usize;
        let clus_bytes = (self.sec_per_clus as usize) * (self.bytes_per_sec as usize);

        while bytes_left > 0 && curr_clus >= 2 {
            let clus_lba = self.cluster_to_lba(curr_clus);
            let sectors_to_read = self.sec_per_clus as u16;

            if dest_offset + clus_bytes <= to_read {
                dev.read_sectors(
                    clus_lba,
                    sectors_to_read,
                    dest_pa + dest_offset as u64,
                    dest_va + dest_offset as u64,
                )?;
                if bytes_left <= clus_bytes {
                    dest_offset += bytes_left;
                    break;
                }
                bytes_left -= clus_bytes;
                dest_offset += clus_bytes;
            } else {
                let mut clus_bytes_left = bytes_left.min(clus_bytes);
                let mut sec_off = 0u64;
                while clus_bytes_left > 0 {
                    let chunk = clus_bytes_left.min(4096);
                    let secs = ((chunk + 511) / 512) as u16;
                    dev.read_sectors(clus_lba + sec_off, secs, scratch_pa, scratch_va)?;
                    core::ptr::copy_nonoverlapping(
                        scratch_va as *const u8,
                        (dest_va + dest_offset as u64) as *mut u8,
                        chunk,
                    );
                    clus_bytes_left -= chunk;
                    dest_offset += chunk;
                    sec_off += secs as u64;
                }
                break;
            }

            if bytes_left == 0 {
                break;
            }

            // Lookup next cluster in FAT
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
            let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
            if curr_clus >= eof {
                break;
            }
        }

        Ok(dest_offset)
    }

    /// Looks up the next cluster in the FAT for cluster `curr_clus`.
    unsafe fn get_next_cluster(
        &self,
        dev: &mut impl BlockDevice,
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

    /// Locates a directory entry in `dir_clus` matching `target_name` (by LFN or 8.3 name).
    pub unsafe fn find_entry_location(
        &self,
        dev: &mut impl BlockDevice,
        dir_clus: u32,
        target_name: &str,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<DirEntryLocation, &'static str> {
        let target_8_3 = to_8_3(target_name);
        let target_short = to_short_8_3(target_name);
        let mut lfn = LfnAccumulator::new();

        if !self.is_fat32 && dir_clus == 0 {
            for s in 0..self.root_dir_sectors {
                let sec = self.root_dir_lba + s as u64;
                dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
                for slot in 0..16 {
                    let entry_ptr = (scratch_va + slot * 32) as *const u8;
                    let fb = *entry_ptr;
                    if fb == 0x00 { return Err("File not found in FAT16 root directory"); }
                    if fb == 0xE5 { lfn.reset(); continue; }
                    let attr = *entry_ptr.add(11);
                    if attr == 0x0F {
                        let mut raw_slot = [0u8; 32];
                        core::ptr::copy_nonoverlapping(entry_ptr, raw_slot.as_mut_ptr(), 32);
                        lfn.process_lfn_slot(&raw_slot);
                        continue;
                    }
                    if (attr & 0x08) != 0 { lfn.reset(); continue; }

                    let mut raw = [0u8; 11];
                    core::ptr::copy_nonoverlapping(entry_ptr, raw.as_mut_ptr(), 11);

                    let mut matches = false;
                    if lfn.valid && lfn_checksum(&raw) == lfn.checksum && lfn.char_len > 0 {
                        let mut lfn_buf = [0u8; 64];
                        let lfn_len = lfn.to_utf8(&mut lfn_buf);
                        if let Ok(lfn_str) = core::str::from_utf8(&lfn_buf[..lfn_len]) {
                            if lfn_str.eq_ignore_ascii_case(target_name) {
                                matches = true;
                            }
                        }
                    }
                    if !matches && raw.eq_ignore_ascii_case(&target_8_3) {
                        matches = true;
                    }
                    if !matches && target_short.is_some() && raw.eq_ignore_ascii_case(&target_short.unwrap()) {
                        matches = true;
                    }
                    let (n83, len83) = format_8_3_name(&raw);
                    if !matches {
                        if let Ok(s83) = core::str::from_utf8(&n83[..len83]) {
                            if s83.eq_ignore_ascii_case(target_name) {
                                matches = true;
                            }
                        }
                    }

                    let lfn_count = lfn.slot_count;
                    lfn.reset();

                    if matches {
                        let is_dir = (attr & 0x10) != 0;
                        let clus_lo = u16::from_le_bytes([*entry_ptr.add(26), *entry_ptr.add(27)]) as u32;
                        let size = u32::from_le_bytes([
                            *entry_ptr.add(28),
                            *entry_ptr.add(29),
                            *entry_ptr.add(30),
                            *entry_ptr.add(31),
                        ]) as usize;
                        return Ok(DirEntryLocation {
                            sec,
                            slot_off: (slot * 32) as usize,
                            lfn_count,
                            first_clus: clus_lo,
                            size,
                            is_dir,
                            short_name: raw,
                        });
                    }
                }
            }
            return Err("File not found in FAT16 root directory");
        }

        let mut curr_clus = dir_clus;
        let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
        while curr_clus >= 2 && curr_clus < eof {
            let start_lba = self.cluster_to_lba(curr_clus);
            for s in 0..self.sec_per_clus as u64 {
                let sec = start_lba + s;
                dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
                for slot in 0..16 {
                    let entry_ptr = (scratch_va + slot * 32) as *const u8;
                    let fb = *entry_ptr;
                    if fb == 0x00 { return Err("File not found in directory"); }
                    if fb == 0xE5 { lfn.reset(); continue; }
                    let attr = *entry_ptr.add(11);
                    if attr == 0x0F {
                        let mut raw_slot = [0u8; 32];
                        core::ptr::copy_nonoverlapping(entry_ptr, raw_slot.as_mut_ptr(), 32);
                        lfn.process_lfn_slot(&raw_slot);
                        continue;
                    }
                    if (attr & 0x08) != 0 { lfn.reset(); continue; }

                    let mut raw = [0u8; 11];
                    core::ptr::copy_nonoverlapping(entry_ptr, raw.as_mut_ptr(), 11);

                    let mut matches = false;
                    if lfn.valid && lfn_checksum(&raw) == lfn.checksum && lfn.char_len > 0 {
                        let mut lfn_buf = [0u8; 64];
                        let lfn_len = lfn.to_utf8(&mut lfn_buf);
                        if let Ok(lfn_str) = core::str::from_utf8(&lfn_buf[..lfn_len]) {
                            if lfn_str.eq_ignore_ascii_case(target_name) {
                                matches = true;
                            }
                        }
                    }
                    if !matches && raw.eq_ignore_ascii_case(&target_8_3) {
                        matches = true;
                    }
                    if !matches && target_short.is_some() && raw.eq_ignore_ascii_case(&target_short.unwrap()) {
                        matches = true;
                    }
                    let (n83, len83) = format_8_3_name(&raw);
                    if !matches {
                        if let Ok(s83) = core::str::from_utf8(&n83[..len83]) {
                            if s83.eq_ignore_ascii_case(target_name) {
                                matches = true;
                            }
                        }
                    }

                    let lfn_count = lfn.slot_count;
                    lfn.reset();

                    if matches {
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
                        return Ok(DirEntryLocation {
                            sec,
                            slot_off: (slot * 32) as usize,
                            lfn_count,
                            first_clus,
                            size,
                            is_dir,
                            short_name: raw,
                        });
                    }
                }
            }
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
        }
        Err("File not found in directory")
    }

    /// Resolves `path` (slash-separated) and returns its directory entry location.
    pub unsafe fn find_entry_location_by_path(
        &self,
        dev: &mut impl BlockDevice,
        path: &str,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<DirEntryLocation, &'static str> {
        let mut path_str = path.trim();
        while path_str.starts_with('/') { path_str = &path_str[1..]; }
        while path_str.ends_with('/') { path_str = &path_str[..path_str.len() - 1]; }
        if path_str.is_empty() { return Err("Empty path"); }

        let mut components = [""; 8];
        let mut comp_count = 0;
        for part in path_str.split('/') {
            if !part.is_empty() {
                if comp_count >= components.len() { return Err("Path depth exceeds 8"); }
                components[comp_count] = part;
                comp_count += 1;
            }
        }
        let mut parent_dir_clus = if self.is_fat32 { self.root_clus } else { 0 };
        for comp in &components[..comp_count - 1] {
            let (entry_clus, _size, is_dir) = self.find_entry_in_dir(dev, parent_dir_clus, comp, scratch_pa, scratch_va)?;
            if !is_dir { return Err("Component is not a directory"); }
            parent_dir_clus = entry_clus;
        }
        self.find_entry_location(dev, parent_dir_clus, components[comp_count - 1], scratch_pa, scratch_va)
    }

    /// Searches directory at `dir_clus` for entry matching `target_name`.
    /// Returns `(first_cluster, file_size, is_dir)`.
    pub unsafe fn find_entry_in_dir(
        &self,
        dev: &mut impl BlockDevice,
        dir_clus: u32,
        target_name: &str,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(u32, usize, bool), &'static str> {
        let loc = self.find_entry_location(dev, dir_clus, target_name, scratch_pa, scratch_va)?;
        Ok((loc.first_clus, loc.size, loc.is_dir))
    }

    /// Enumerates directory entries in directory cluster `dir_clus` (or root if 0 on FAT16).
    pub unsafe fn list_dir_cluster<F: FnMut(&FatDirEntry)>(
        &self,
        dev: &mut impl BlockDevice,
        dir_clus: u32,
        scratch_pa: u64,
        scratch_va: u64,
        mut callback: F,
    ) -> Result<(), &'static str> {
        let mut lfn = LfnAccumulator::new();

        if !self.is_fat32 && dir_clus == 0 {
            for s in 0..self.root_dir_sectors {
                let sec = self.root_dir_lba + s as u64;
                dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
                if self.scan_dir_sector_entries_lfn(scratch_va, &mut lfn, &mut callback) {
                    break;
                }
            }
            return Ok(());
        }

        let mut curr_clus = dir_clus;
        let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
        while curr_clus >= 2 && curr_clus < eof {
            let start_lba = self.cluster_to_lba(curr_clus);
            for s in 0..self.sec_per_clus as u64 {
                dev.read_sectors(start_lba + s, 1, scratch_pa, scratch_va)?;
                if self.scan_dir_sector_entries_lfn(scratch_va, &mut lfn, &mut callback) {
                    return Ok(());
                }
            }
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
        }

        Ok(())
    }

    /// Resolves `path` (or root if empty or "/") and enumerates all directory entries.
    pub unsafe fn list_dir<F: FnMut(&FatDirEntry)>(
        &self,
        dev: &mut impl BlockDevice,
        path: &str,
        scratch_pa: u64,
        scratch_va: u64,
        callback: F,
    ) -> Result<(), &'static str> {
        let mut path_str = path.trim();
        while path_str.starts_with('/') {
            path_str = &path_str[1..];
        }
        while path_str.ends_with('/') {
            path_str = &path_str[..path_str.len() - 1];
        }

        let root_clus = if self.is_fat32 { self.root_clus } else { 0 };
        if path_str.is_empty() || path_str == "." {
            return self.list_dir_cluster(dev, root_clus, scratch_pa, scratch_va, callback);
        }

        let mut curr_dir_clus = root_clus;
        let mut components = [""; 8];
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

        for comp in &components[..comp_count] {
            let (entry_clus, _size, is_dir) = self.find_entry_in_dir(
                dev,
                curr_dir_clus,
                comp,
                scratch_pa,
                scratch_va,
            )?;
            if !is_dir {
                return Err("Path component is not a directory");
            }
            curr_dir_clus = entry_clus;
        }

        self.list_dir_cluster(dev, curr_dir_clus, scratch_pa, scratch_va, callback)
    }

    /// Scans a 512-byte directory sector at `scratch_va` using `lfn` and passes valid entries to `callback`.
    /// Returns `true` if end-of-directory marker (0x00) is encountered.
    pub unsafe fn scan_dir_sector_entries_lfn<F: FnMut(&FatDirEntry)>(
        &self,
        scratch_va: u64,
        lfn: &mut LfnAccumulator,
        callback: &mut F,
    ) -> bool {
        for e in 0..16 {
            let entry_ptr = (scratch_va + e * 32) as *const u8;
            let first_byte = *entry_ptr;
            if first_byte == 0x00 {
                lfn.reset();
                return true;
            }
            if first_byte == 0xE5 {
                lfn.reset();
                continue;
            }

            let attr = *entry_ptr.add(11);
            if attr == 0x0F {
                let mut raw_slot = [0u8; 32];
                core::ptr::copy_nonoverlapping(entry_ptr, raw_slot.as_mut_ptr(), 32);
                lfn.process_lfn_slot(&raw_slot);
                continue;
            }
            if (attr & 0x08) != 0 {
                lfn.reset();
                continue;
            }

            let mut raw = [0u8; 11];
            core::ptr::copy_nonoverlapping(entry_ptr, raw.as_mut_ptr(), 11);

            let mut name = [0u8; 64];
            let name_len = if lfn.valid && lfn_checksum(&raw) == lfn.checksum && lfn.char_len > 0 {
                lfn.to_utf8(&mut name)
            } else {
                let (name_8_3, len_8_3) = format_8_3_name(&raw);
                name[..len_8_3].copy_from_slice(&name_8_3[..len_8_3]);
                len_8_3
            };
            lfn.reset();

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

            let entry = FatDirEntry {
                name,
                name_len,
                size,
                is_dir,
                first_clus,
            };
            callback(&entry);
        }
        false
    }

    /// Scans a 512-byte directory sector at `scratch_va` and passes valid entries to `callback`.
    /// Returns `true` if end-of-directory marker (0x00) is encountered.
    pub unsafe fn scan_dir_sector_entries<F: FnMut(&FatDirEntry)>(
        &self,
        scratch_va: u64,
        callback: &mut F,
    ) -> bool {
        let mut lfn = LfnAccumulator::new();
        self.scan_dir_sector_entries_lfn(scratch_va, &mut lfn, callback)
    }

    /// Scans the FAT table for the first free cluster (entry == 0) starting at `start_clus`.
    pub unsafe fn find_free_cluster(
        &self,
        dev: &mut impl BlockDevice,
        start_clus: u32,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<u32, &'static str> {
        let entry_size = if self.is_fat32 { 4u64 } else { 2u64 };
        let entries_per_sec = 512 / entry_size;
        let start_c = start_clus.max(2);
        let start_sec = ((start_c as u64) * entry_size) / 512;

        for sec_idx in start_sec..self.fat_size as u64 {
            let sec_lba = self.fat_start_lba + sec_idx;
            dev.read_sectors(sec_lba, 1, scratch_pa, scratch_va)?;

            for i in 0..entries_per_sec {
                let clus = (sec_idx * entries_per_sec + i) as u32;
                if clus < start_c {
                    continue;
                }
                if self.is_fat32 {
                    let ptr = (scratch_va + i * 4) as *const u8;
                    let val = u32::from_le_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]) & 0x0FFF_FFFF;
                    if val == 0 {
                        return Ok(clus);
                    }
                } else {
                    let ptr = (scratch_va + i * 2) as *const u8;
                    let val = u16::from_le_bytes([*ptr, *ptr.add(1)]);
                    if val == 0 {
                        return Ok(clus);
                    }
                }
            }
        }

        Err("No free clusters available in FAT volume")
    }

    /// Sets the FAT entry for cluster `clus` to `next_val` in all FAT copies.
    pub unsafe fn set_fat_entry(
        &self,
        dev: &mut impl BlockDevice,
        clus: u32,
        next_val: u32,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let entry_size = if self.is_fat32 { 4u64 } else { 2u64 };
        let fat_byte_off = (clus as u64) * entry_size;
        let sec_offset = fat_byte_off / 512;
        let ent_off = (fat_byte_off % 512) as usize;

        for f in 0..self.num_fats {
            let fat_start = self.fat_start_lba + (f as u64) * (self.fat_size as u64);
            let sec = fat_start + sec_offset;
            dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;

            if self.is_fat32 {
                let ptr = (scratch_va + ent_off as u64) as *mut u8;
                let old = u32::from_le_bytes([*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)]);
                let updated = (old & 0xF000_0000) | (next_val & 0x0FFF_FFFF);
                let bytes = updated.to_le_bytes();
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, 4);
            } else {
                let ptr = (scratch_va + ent_off as u64) as *mut u8;
                let bytes = (next_val as u16).to_le_bytes();
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, 2);
            }

            dev.write_sectors(sec, 1, scratch_pa, scratch_va)?;
        }

        Ok(())
    }

    /// Frees an allocated cluster chain starting at `start_clus` by clearing all FAT entries to 0.
    pub unsafe fn free_cluster_chain(
        &self,
        dev: &mut impl BlockDevice,
        start_clus: u32,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let mut curr = start_clus;
        let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
        while curr >= 2 && curr < eof {
            let next = self.get_next_cluster(dev, curr, scratch_pa, scratch_va)?;
            self.set_fat_entry(dev, curr, 0, scratch_pa, scratch_va)?;
            curr = next;
        }
        Ok(())
    }

    /// Finds `num_slots` consecutive 32-byte slots in `dir_clus` to write directory entries.
    /// If directory is full and FAT32, allocates a new cluster.
    unsafe fn find_dir_slots_for_create(
        &self,
        dev: &mut impl BlockDevice,
        dir_clus: u32,
        num_slots: usize,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(u64, usize), &'static str> {
        if !self.is_fat32 && dir_clus == 0 {
            for s in 0..self.root_dir_sectors {
                let sec = self.root_dir_lba + s as u64;
                dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
                let mut run = 0;
                let mut start_idx = 0;
                for slot in 0..16 {
                    let fb = *((scratch_va + slot * 32) as *const u8);
                    if fb == 0x00 || fb == 0xE5 {
                        if run == 0 { start_idx = slot; }
                        run += 1;
                        if run >= num_slots {
                            return Ok((sec, (start_idx * 32) as usize));
                        }
                    } else {
                        run = 0;
                    }
                }
            }
            return Err("FAT16 root directory full");
        }

        let mut curr_clus = dir_clus;
        let mut last_clus = dir_clus;
        let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
        while curr_clus >= 2 && curr_clus < eof {
            last_clus = curr_clus;
            let start_lba = self.cluster_to_lba(curr_clus);
            for s in 0..self.sec_per_clus as u64 {
                let sec = start_lba + s;
                dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
                let mut run = 0;
                let mut start_idx = 0;
                for slot in 0..16 {
                    let fb = *((scratch_va + slot * 32) as *const u8);
                    if fb == 0x00 || fb == 0xE5 {
                        if run == 0 { start_idx = slot; }
                        run += 1;
                        if run >= num_slots {
                            return Ok((sec, (start_idx * 32) as usize));
                        }
                    } else {
                        run = 0;
                    }
                }
            }
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
        }

        // Allocate a new cluster for directory if full
        if self.is_fat32 && last_clus >= 2 {
            let new_clus = self.find_free_cluster(dev, 2, scratch_pa, scratch_va)?;
            let eof_val = 0x0FFF_FFFFu32;
            self.set_fat_entry(dev, last_clus, new_clus, scratch_pa, scratch_va)?;
            self.set_fat_entry(dev, new_clus, eof_val, scratch_pa, scratch_va)?;

            let new_lba = self.cluster_to_lba(new_clus);
            core::ptr::write_bytes(scratch_va as *mut u8, 0, 512);
            for s in 0..self.sec_per_clus as u64 {
                dev.write_sectors(new_lba + s, 1, scratch_pa, scratch_va)?;
            }
            return Ok((new_lba, 0));
        }

        Err("Directory full (no free slots)")
    }

    /// Creates a directory entry (with VFAT LFN if needed) in `parent_dir_clus`.
    pub unsafe fn create_dir_entry(
        &self,
        dev: &mut impl BlockDevice,
        parent_dir_clus: u32,
        name: &str,
        first_clus: u32,
        size: usize,
        is_dir: bool,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let short_opt = to_short_8_3(name);
        let is_lfn = short_opt.is_some() || name.len() > 12;

        if !is_lfn {
            let short_8_3 = to_8_3(name);
            let (sec, slot_off) = self.find_dir_slots_for_create(dev, parent_dir_clus, 1, scratch_pa, scratch_va)?;
            dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;
            let entry_ptr = (scratch_va + slot_off as u64) as *mut u8;
            let was_zero = *entry_ptr == 0x00;
            core::ptr::write_bytes(entry_ptr, 0, 32);
            core::ptr::copy_nonoverlapping(short_8_3.as_ptr(), entry_ptr, 11);
            *entry_ptr.add(11) = if is_dir { 0x10 } else { 0x20 };

            let clus_hi = (first_clus >> 16) as u16;
            let clus_lo = (first_clus & 0xFFFF) as u16;
            if self.is_fat32 {
                let b_hi = clus_hi.to_le_bytes();
                core::ptr::copy_nonoverlapping(b_hi.as_ptr(), entry_ptr.add(20), 2);
            }
            let b_lo = clus_lo.to_le_bytes();
            core::ptr::copy_nonoverlapping(b_lo.as_ptr(), entry_ptr.add(26), 2);

            let sz_bytes = (size as u32).to_le_bytes();
            core::ptr::copy_nonoverlapping(sz_bytes.as_ptr(), entry_ptr.add(28), 4);

            if was_zero && slot_off + 32 < 512 {
                *(entry_ptr.add(32)) = 0x00;
            }
            dev.write_sectors(sec, 1, scratch_pa, scratch_va)?;
            Ok(())
        } else {
            let short_8_3 = short_opt.unwrap_or_else(|| to_8_3(name));
            let chk = lfn_checksum(&short_8_3);

            let mut chars = [0u16; 64];
            let mut char_count = 0;
            for ch in name.chars() {
                if char_count < 64 {
                    chars[char_count] = ch as u16;
                    char_count += 1;
                }
            }

            let num_lfn = (char_count + 13) / 13;
            let num_slots = num_lfn + 1;

            let (sec, slot_off) = self.find_dir_slots_for_create(dev, parent_dir_clus, num_slots, scratch_pa, scratch_va)?;
            dev.read_sectors(sec, 1, scratch_pa, scratch_va)?;

            let last_slot_off = slot_off + (num_slots - 1) * 32;
            let was_zero = *((scratch_va + last_slot_off as u64) as *const u8) == 0x00;

            for idx in 0..num_lfn {
                let seq = num_lfn - idx;
                let is_last = idx == 0;
                let start_c = (seq - 1) * 13;
                let chunk = if start_c < char_count { &chars[start_c..char_count] } else { &[] };
                let lfn_bytes = build_lfn_entry(seq as u8, is_last, chk, chunk);
                let ptr = (scratch_va + (slot_off + idx * 32) as u64) as *mut u8;
                core::ptr::copy_nonoverlapping(lfn_bytes.as_ptr(), ptr, 32);
            }

            let short_ptr = (scratch_va + (slot_off + num_lfn * 32) as u64) as *mut u8;
            core::ptr::write_bytes(short_ptr, 0, 32);
            core::ptr::copy_nonoverlapping(short_8_3.as_ptr(), short_ptr, 11);
            *short_ptr.add(11) = if is_dir { 0x10 } else { 0x20 };

            let clus_hi = (first_clus >> 16) as u16;
            let clus_lo = (first_clus & 0xFFFF) as u16;
            if self.is_fat32 {
                let b_hi = clus_hi.to_le_bytes();
                core::ptr::copy_nonoverlapping(b_hi.as_ptr(), short_ptr.add(20), 2);
            }
            let b_lo = clus_lo.to_le_bytes();
            core::ptr::copy_nonoverlapping(b_lo.as_ptr(), short_ptr.add(26), 2);

            let sz_bytes = (size as u32).to_le_bytes();
            core::ptr::copy_nonoverlapping(sz_bytes.as_ptr(), short_ptr.add(28), 4);

            if was_zero && slot_off + num_slots * 32 < 512 {
                *((scratch_va + (slot_off + num_slots * 32) as u64) as *mut u8) = 0x00;
            }

            dev.write_sectors(sec, 1, scratch_pa, scratch_va)?;
            Ok(())
        }
    }

    /// Writes `data` to a file identified by `path`.
    /// Creates or updates a directory entry (with VFAT LFN if needed), allocates clusters in the FAT,
    /// writes file data to sectors, and updates the directory entry with file size.
    pub unsafe fn write_file(
        &self,
        dev: &mut impl BlockDevice,
        path: &str,
        data: &[u8],
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let mut path_str = path.trim();
        while path_str.starts_with('/') { path_str = &path_str[1..]; }
        while path_str.ends_with('/') { path_str = &path_str[..path_str.len() - 1]; }
        if path_str.is_empty() { return Err("Empty filename specified"); }

        let mut components = [""; 8];
        let mut comp_count = 0;
        for part in path_str.split('/') {
            if !part.is_empty() {
                if comp_count >= components.len() { return Err("Path depth exceeds 8"); }
                components[comp_count] = part;
                comp_count += 1;
            }
        }

        let mut parent_dir_clus = if self.is_fat32 { self.root_clus } else { 0 };
        for comp in &components[..comp_count - 1] {
            let (entry_clus, _size, is_dir) = self.find_entry_in_dir(dev, parent_dir_clus, comp, scratch_pa, scratch_va)?;
            if !is_dir { return Err("Parent path component is not a directory"); }
            parent_dir_clus = entry_clus;
        }

        let file_name = components[comp_count - 1];
        let existing_loc = self.find_entry_location(dev, parent_dir_clus, file_name, scratch_pa, scratch_va).ok();

        if let Some(ref loc) = existing_loc {
            if loc.is_dir { return Err("Target is a directory"); }
            if loc.first_clus >= 2 {
                self.free_cluster_chain(dev, loc.first_clus, scratch_pa, scratch_va)?;
            }
        }

        // Allocate new clusters for data
        let clus_bytes = (self.sec_per_clus as usize) * (self.bytes_per_sec as usize);
        let num_clus = if data.is_empty() { 0 } else { (data.len() + clus_bytes - 1) / clus_bytes };

        let mut first_clus = 0u32;
        let mut prev_clus = 0u32;
        let mut search_start = 2u32;
        let eof_val = if self.is_fat32 { 0x0FFF_FFFF } else { 0xFFFF };

        for i in 0..num_clus {
            let free = self.find_free_cluster(dev, search_start, scratch_pa, scratch_va)?;
            if i == 0 {
                first_clus = free;
            } else {
                self.set_fat_entry(dev, prev_clus, free, scratch_pa, scratch_va)?;
            }
            self.set_fat_entry(dev, free, eof_val, scratch_pa, scratch_va)?;
            prev_clus = free;
            search_start = free + 1;
        }

        // Write data sectors
        let mut curr_clus = first_clus;
        let mut file_off = 0usize;
        while file_off < data.len() && curr_clus >= 2 {
            let clus_lba = self.cluster_to_lba(curr_clus);
            for s in 0..self.sec_per_clus as u64 {
                core::ptr::write_bytes(scratch_va as *mut u8, 0, 512);
                if file_off < data.len() {
                    let chunk = (data.len() - file_off).min(512);
                    core::ptr::copy_nonoverlapping(
                        data[file_off..].as_ptr(),
                        scratch_va as *mut u8,
                        chunk,
                    );
                    file_off += chunk;
                }
                dev.write_sectors(clus_lba + s, 1, scratch_pa, scratch_va)?;
            }
            if file_off >= data.len() {
                break;
            }
            curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
        }

        if let Some(ref loc) = existing_loc {
            dev.read_sectors(loc.sec, 1, scratch_pa, scratch_va)?;
            let entry_ptr = (scratch_va + loc.slot_off as u64) as *mut u8;
            let clus_hi = (first_clus >> 16) as u16;
            let clus_lo = (first_clus & 0xFFFF) as u16;
            if self.is_fat32 {
                core::ptr::copy_nonoverlapping(clus_hi.to_le_bytes().as_ptr(), entry_ptr.add(20), 2);
            }
            core::ptr::copy_nonoverlapping(clus_lo.to_le_bytes().as_ptr(), entry_ptr.add(26), 2);
            let sz_bytes = (data.len() as u32).to_le_bytes();
            core::ptr::copy_nonoverlapping(sz_bytes.as_ptr(), entry_ptr.add(28), 4);
            dev.write_sectors(loc.sec, 1, scratch_pa, scratch_va)?;
        } else {
            self.create_dir_entry(dev, parent_dir_clus, file_name, first_clus, data.len(), false, scratch_pa, scratch_va)?;
        }

        Ok(())
    }

    /// Creates a new directory at `path` (with `.` and `..` sub-entries).
    pub unsafe fn mkdir(
        &self,
        dev: &mut impl BlockDevice,
        path: &str,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let mut path_str = path.trim();
        while path_str.starts_with('/') { path_str = &path_str[1..]; }
        while path_str.ends_with('/') { path_str = &path_str[..path_str.len() - 1]; }
        if path_str.is_empty() { return Err("Empty directory name"); }

        let mut components = [""; 8];
        let mut comp_count = 0;
        for part in path_str.split('/') {
            if !part.is_empty() {
                if comp_count >= components.len() { return Err("Path depth exceeds 8"); }
                components[comp_count] = part;
                comp_count += 1;
            }
        }
        let mut parent_dir_clus = if self.is_fat32 { self.root_clus } else { 0 };
        for comp in &components[..comp_count - 1] {
            let (entry_clus, _size, is_dir) = self.find_entry_in_dir(dev, parent_dir_clus, comp, scratch_pa, scratch_va)?;
            if !is_dir { return Err("Component is not a directory"); }
            parent_dir_clus = entry_clus;
        }

        let dir_name = components[comp_count - 1];
        if self.find_entry_in_dir(dev, parent_dir_clus, dir_name, scratch_pa, scratch_va).is_ok() {
            return Err("File or directory already exists");
        }

        let new_clus = self.find_free_cluster(dev, 2, scratch_pa, scratch_va)?;
        let eof_val = if self.is_fat32 { 0x0FFF_FFFF } else { 0xFFFF };
        self.set_fat_entry(dev, new_clus, eof_val, scratch_pa, scratch_va)?;

        let sec_lba = self.cluster_to_lba(new_clus);
        core::ptr::write_bytes(scratch_va as *mut u8, 0, 512);

        // "."
        let dot = scratch_va as *mut u8;
        core::ptr::copy_nonoverlapping(b".          ".as_ptr(), dot, 11);
        *dot.add(11) = 0x10;
        let clus_hi = (new_clus >> 16) as u16;
        let clus_lo = (new_clus & 0xFFFF) as u16;
        if self.is_fat32 {
            core::ptr::copy_nonoverlapping(clus_hi.to_le_bytes().as_ptr(), dot.add(20), 2);
        }
        core::ptr::copy_nonoverlapping(clus_lo.to_le_bytes().as_ptr(), dot.add(26), 2);

        // ".."
        let dotdot = (scratch_va + 32) as *mut u8;
        core::ptr::copy_nonoverlapping(b"..         ".as_ptr(), dotdot, 11);
        *dotdot.add(11) = 0x10;
        let p_clus = if parent_dir_clus == self.root_clus { 0 } else { parent_dir_clus };
        let p_hi = (p_clus >> 16) as u16;
        let p_lo = (p_clus & 0xFFFF) as u16;
        if self.is_fat32 {
            core::ptr::copy_nonoverlapping(p_hi.to_le_bytes().as_ptr(), dotdot.add(20), 2);
        }
        core::ptr::copy_nonoverlapping(p_lo.to_le_bytes().as_ptr(), dotdot.add(26), 2);

        dev.write_sectors(sec_lba, 1, scratch_pa, scratch_va)?;

        if self.sec_per_clus > 1 {
            core::ptr::write_bytes(scratch_va as *mut u8, 0, 512);
            for s in 1..self.sec_per_clus as u64 {
                dev.write_sectors(sec_lba + s, 1, scratch_pa, scratch_va)?;
            }
        }

        self.create_dir_entry(dev, parent_dir_clus, dir_name, new_clus, 0, true, scratch_pa, scratch_va)
    }

    /// Deletes a file or empty directory at `path`.
    pub unsafe fn remove(
        &self,
        dev: &mut impl BlockDevice,
        path: &str,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let loc = self.find_entry_location_by_path(dev, path, scratch_pa, scratch_va)?;

        if loc.is_dir {
            let mut curr_clus = loc.first_clus;
            let eof = if self.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
            while curr_clus >= 2 && curr_clus < eof {
                let start_lba = self.cluster_to_lba(curr_clus);
                for s in 0..self.sec_per_clus as u64 {
                    dev.read_sectors(start_lba + s, 1, scratch_pa, scratch_va)?;
                    for slot in 0..16 {
                        let entry_ptr = (scratch_va + slot * 32) as *const u8;
                        let fb = *entry_ptr;
                        if fb == 0x00 { break; }
                        if fb == 0xE5 { continue; }
                        let attr = *entry_ptr.add(11);
                        if attr == 0x0F { continue; }
                        let name_slice = core::slice::from_raw_parts(entry_ptr, 11);
                        if name_slice == b".          " || name_slice == b"..         " {
                            continue;
                        }
                        return Err("Directory not empty");
                    }
                }
                curr_clus = self.get_next_cluster(dev, curr_clus, scratch_pa, scratch_va)?;
            }
        }

        dev.read_sectors(loc.sec, 1, scratch_pa, scratch_va)?;
        let entry_ptr = (scratch_va + loc.slot_off as u64) as *mut u8;
        *entry_ptr = 0xE5;
        for i in 1..=loc.lfn_count {
            if loc.slot_off >= i * 32 {
                *entry_ptr.sub(i * 32) = 0xE5;
            }
        }
        dev.write_sectors(loc.sec, 1, scratch_pa, scratch_va)?;

        if loc.first_clus >= 2 {
            self.free_cluster_chain(dev, loc.first_clus, scratch_pa, scratch_va)?;
        }

        Ok(())
    }

    /// Copies a file from `src` to `dst`.
    pub unsafe fn copy_file(
        &self,
        dev: &mut impl BlockDevice,
        src: &str,
        dst: &str,
        buf_pa: u64,
        buf_va: u64,
        buf_cap: usize,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<usize, &'static str> {
        let bytes = self.read_file(dev, src, buf_pa, buf_va, buf_cap, scratch_pa, scratch_va)?;
        let slice = core::slice::from_raw_parts(buf_va as *const u8, bytes);
        self.write_file(dev, dst, slice, scratch_pa, scratch_va)?;
        Ok(bytes)
    }

    /// Moves or renames a file or directory from `src` to `dst`.
    pub unsafe fn move_file(
        &self,
        dev: &mut impl BlockDevice,
        src: &str,
        dst: &str,
        _buf_pa: u64,
        _buf_va: u64,
        _buf_cap: usize,
        scratch_pa: u64,
        scratch_va: u64,
    ) -> Result<(), &'static str> {
        let src_clean = src.trim();
        let dst_clean = dst.trim();
        if src_clean == dst_clean {
            return Ok(());
        }

        let loc = self.find_entry_location_by_path(dev, src_clean, scratch_pa, scratch_va)?;

        let mut dst_str = dst_clean;
        while dst_str.starts_with('/') { dst_str = &dst_str[1..]; }
        while dst_str.ends_with('/') { dst_str = &dst_str[..dst_str.len() - 1]; }
        if dst_str.is_empty() { return Err("Empty destination path"); }

        let mut components = [""; 8];
        let mut comp_count = 0;
        for part in dst_str.split('/') {
            if !part.is_empty() {
                if comp_count >= components.len() { return Err("Path depth exceeds 8"); }
                components[comp_count] = part;
                comp_count += 1;
            }
        }
        let mut dst_parent_clus = if self.is_fat32 { self.root_clus } else { 0 };
        for comp in &components[..comp_count - 1] {
            let (entry_clus, _size, is_dir) = self.find_entry_in_dir(dev, dst_parent_clus, comp, scratch_pa, scratch_va)?;
            if !is_dir { return Err("Destination parent is not a directory"); }
            dst_parent_clus = entry_clus;
        }
        let dst_name = components[comp_count - 1];

        if self.find_entry_in_dir(dev, dst_parent_clus, dst_name, scratch_pa, scratch_va).is_ok() {
            self.remove(dev, dst_clean, scratch_pa, scratch_va)?;
        }

        // Mark old entry 0xE5 first so virtual FAT layers (e.g. QEMU vvfat)
        // know the cluster mapping is deleted before relinking.
        dev.read_sectors(loc.sec, 1, scratch_pa, scratch_va)?;
        let entry_ptr = (scratch_va + loc.slot_off as u64) as *mut u8;
        *entry_ptr = 0xE5;
        for i in 1..=loc.lfn_count {
            if loc.slot_off >= i * 32 {
                *entry_ptr.sub(i * 32) = 0xE5;
            }
        }
        dev.write_sectors(loc.sec, 1, scratch_pa, scratch_va)?;

        self.create_dir_entry(
            dev,
            dst_parent_clus,
            dst_name,
            loc.first_clus,
            loc.size,
            loc.is_dir,
            scratch_pa,
            scratch_va,
        )?;

        if loc.is_dir && loc.first_clus >= 2 {
            let dir_sec = self.cluster_to_lba(loc.first_clus);
            dev.read_sectors(dir_sec, 1, scratch_pa, scratch_va)?;
            let p_clus = if dst_parent_clus == self.root_clus { 0 } else { dst_parent_clus };
            let p_hi = (p_clus >> 16) as u16;
            let p_lo = (p_clus & 0xFFFF) as u16;
            if self.is_fat32 {
                core::ptr::copy_nonoverlapping(p_hi.to_le_bytes().as_ptr(), (scratch_va as *mut u8).add(52), 2);
            }
            core::ptr::copy_nonoverlapping(p_lo.to_le_bytes().as_ptr(), (scratch_va as *mut u8).add(58), 2);
            dev.write_sectors(dir_sec, 1, scratch_pa, scratch_va)?;
        }

        Ok(())
    }
}

/// A parsed FAT directory entry supporting both 8.3 and VFAT Long File Names (LFN).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FatDirEntry {
    pub name: [u8; 64],
    pub name_len: usize,
    pub size: usize,
    pub is_dir: bool,
    pub first_clus: u32,
}

impl FatDirEntry {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("???")
    }
}

/// Converts raw 11-byte space-padded 8.3 directory name into human-readable ASCII string.
/// E.g. `b"WEIGHTS BIN"` -> `("WEIGHTS.BIN", 11)`, `b"EFI        "` -> `("EFI", 3)`.
pub fn format_8_3_name(raw: &[u8; 11]) -> ([u8; 13], usize) {
    let mut out = [0u8; 13];
    let mut len = 0;

    // Base name (up to 8 bytes, space-trimmed)
    let mut base_len = 8;
    while base_len > 0 && raw[base_len - 1] == b' ' {
        base_len -= 1;
    }
    for i in 0..base_len {
        out[len] = raw[i];
        len += 1;
    }

    // Extension (up to 3 bytes, space-trimmed)
    let mut ext_len = 3;
    while ext_len > 0 && raw[8 + ext_len - 1] == b' ' {
        ext_len -= 1;
    }
    if ext_len > 0 {
        out[len] = b'.';
        len += 1;
        for i in 0..ext_len {
            out[len] = raw[8 + i];
            len += 1;
        }
    }

    (out, len)
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

/// Generates the standard DOS/VFAT 8.3 numeric tail representation for long filenames.
/// E.g. `"TRANSFORMER.BIN"` -> `Some(b"TRANSF~1BIN")`.
pub fn to_short_8_3(name: &str) -> Option<[u8; 11]> {
    let mut parts = name.splitn(2, '.');
    let base = parts.next().unwrap_or("");
    let ext = parts.next().unwrap_or("");
    if base.len() <= 8 && ext.len() <= 3 && !base.contains('~') && !name.contains(' ') {
        return None;
    }
    let mut res = [b' '; 11];
    let mut b_idx = 0;
    for ch in base.bytes() {
        if b_idx < 6 && ch != b' ' && ch != b'.' {
            res[b_idx] = ch.to_ascii_uppercase();
            b_idx += 1;
        }
    }
    if b_idx < 8 {
        res[b_idx] = b'~';
        res[b_idx + 1] = b'1';
    }

    let mut e_idx = 8;
    for ch in ext.bytes().take(3) {
        if ch != b' ' {
            res[e_idx] = ch.to_ascii_uppercase();
            e_idx += 1;
        }
    }
    Some(res)
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
        assert_eq!(to_short_8_3("TRANSFORMER.BIN"), Some(*b"TRANSF~1BIN"));
        assert_eq!(to_short_8_3("HELLO.ELF"), None);
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

    #[test]
    fn format_8_3_name_roundtrip_test() {
        assert_eq!(format_8_3_name(b"WEIGHTS BIN"), (*b"WEIGHTS.BIN\0\0", 11));
        assert_eq!(format_8_3_name(b"EFI        "), (*b"EFI\0\0\0\0\0\0\0\0\0\0", 3));
        assert_eq!(format_8_3_name(b".          "), (*b".\0\0\0\0\0\0\0\0\0\0\0\0", 1));
        assert_eq!(format_8_3_name(b"..         "), (*b"..\0\0\0\0\0\0\0\0\0\0\0", 2));
        assert_eq!(format_8_3_name(b"A       B  "), (*b"A.B\0\0\0\0\0\0\0\0\0\0", 3));
    }

    #[test]
    fn fat16_and_fat32_eof_test() {
        let fat32_vol = FatVolume {
            volume_lba: 0,
            bytes_per_sec: 512,
            sec_per_clus: 1,
            rsvd_sec_cnt: 32,
            num_fats: 2,
            fat_size: 100,
            root_clus: 2,
            fat_start_lba: 32,
            data_start_lba: 232,
            is_fat32: true,
            root_dir_lba: 0,
            root_dir_sectors: 0,
        };
        let fat16_vol = FatVolume {
            volume_lba: 0,
            bytes_per_sec: 512,
            sec_per_clus: 1,
            rsvd_sec_cnt: 1,
            num_fats: 2,
            fat_size: 16,
            root_clus: 0,
            fat_start_lba: 1,
            data_start_lba: 65,
            is_fat32: false,
            root_dir_lba: 33,
            root_dir_sectors: 32,
        };

        let eof32 = if fat32_vol.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };
        let eof16 = if fat16_vol.is_fat32 { 0x0FFF_FFF8 } else { 0xFFF8 };

        assert_eq!(eof32, 0x0FFF_FFF8);
        assert_eq!(eof16, 0xFFF8);

        // In FAT16, 0xFFF8..=0xFFFF are EOF markers
        assert!(0xFFF8 >= eof16);
        assert!(0xFFFF >= eof16);
        assert!(0x0010 < eof16);

        // In FAT32, 0x0FFF_FFF8..=0x0FFF_FFFF are EOF markers
        assert!(0x0FFF_FFF8 >= eof32);
        assert!(0x0FFF_FFFF >= eof32);
        assert!(0x0000_1234 < eof32);
    }

    #[test]
    fn list_dir_scan_sector_test() {
        let vol = FatVolume {
            volume_lba: 0,
            bytes_per_sec: 512,
            sec_per_clus: 1,
            rsvd_sec_cnt: 32,
            num_fats: 2,
            fat_size: 100,
            root_clus: 2,
            fat_start_lba: 32,
            data_start_lba: 232,
            is_fat32: true,
            root_dir_lba: 0,
            root_dir_sectors: 0,
        };

        // Construct a mock directory sector with 3 entries:
        // 1. "EFI" directory (attr 0x10, clus 3)
        // 2. Deleted entry (0xE5)
        // 3. "WEIGHTS.BIN" file (attr 0x20, clus 5, size 32768)
        // 4. End-of-dir marker (0x00)
        let mut sec = [0u8; 512];

        // Entry 0: EFI directory
        sec[0..11].copy_from_slice(b"EFI        ");
        sec[11] = 0x10; // Directory
        sec[26] = 3;    // Cluster lo = 3

        // Entry 1: Deleted entry
        sec[32] = 0xE5;

        // Entry 2: WEIGHTS.BIN
        sec[64..75].copy_from_slice(b"WEIGHTS BIN");
        sec[75] = 0x20; // Archive / regular file
        sec[90] = 5;    // Cluster lo = 5
        let size_bytes = (32768u32).to_le_bytes();
        sec[92..96].copy_from_slice(&size_bytes);

        // Entry 3: End of directory (0x00 already)

        let mut count = 0usize;
        let mut names = [""; 4];
        let mut sizes = [0usize; 4];
        let mut is_dirs = [false; 4];

        unsafe {
            let done = vol.scan_dir_sector_entries(sec.as_ptr() as u64, &mut |e| {
                if count < 4 {
                    // Safety for test: string slice is valid as long as e.name is
                    // But e.name is copied into local array so convert to static str comparison
                    if e.name_str() == "EFI" {
                        names[count] = "EFI";
                    } else if e.name_str() == "WEIGHTS.BIN" {
                        names[count] = "WEIGHTS.BIN";
                    }
                    sizes[count] = e.size;
                    is_dirs[count] = e.is_dir;
                    count += 1;
                }
            });
            assert!(done, "Should have reached end of directory marker");
        }

        assert_eq!(count, 2);
        assert_eq!(names[0], "EFI");
        assert_eq!(sizes[0], 0);
        assert_eq!(is_dirs[0], true);
        assert_eq!(names[1], "WEIGHTS.BIN");
        assert_eq!(sizes[1], 32768);
        assert_eq!(is_dirs[1], false);
    }

    struct RamDisk {
        data: alloc::vec::Vec<u8>,
    }

    impl RamDisk {
        fn new(size_bytes: usize) -> Self {
            Self {
                data: alloc::vec![0u8; size_bytes],
            }
        }
    }

    impl BlockDevice for RamDisk {
        unsafe fn read_blocks(&mut self, lba: u64, count: u16, _buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
            let offset = (lba * 512) as usize;
            let len = (count as usize) * 512;
            if offset + len > self.data.len() {
                return Err("RamDisk out of bounds read");
            }
            core::ptr::copy_nonoverlapping(self.data[offset..].as_ptr(), buf_va as *mut u8, len);
            Ok(())
        }

        unsafe fn write_blocks(&mut self, lba: u64, count: u16, _buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
            let offset = (lba * 512) as usize;
            let len = (count as usize) * 512;
            if offset + len > self.data.len() {
                return Err("RamDisk out of bounds write");
            }
            core::ptr::copy_nonoverlapping(buf_va as *const u8, self.data[offset..].as_mut_ptr(), len);
            Ok(())
        }

        fn block_size(&self) -> u32 { 512 }
        fn total_blocks(&self) -> u64 { (self.data.len() / 512) as u64 }
        fn name(&self) -> &str { "ramdisk" }
    }

    #[test]
    fn fat32_write_file_and_read_back_roundtrip_test() {
        unsafe {
            let mut dev = RamDisk::new(2 * 1024 * 1024);
            let disk_ptr = dev.data.as_mut_ptr();

            let vol = FatVolume {
                volume_lba: 0,
                bytes_per_sec: 512,
                sec_per_clus: 1,
                rsvd_sec_cnt: 32,
                num_fats: 2,
                fat_size: 32,
                root_clus: 2,
                fat_start_lba: 32,
                data_start_lba: 96,
                is_fat32: true,
                root_dir_lba: 0,
                root_dir_sectors: 0,
            };

            // Initialize FAT1 and FAT2 cluster entries 0, 1, 2 (root directory EOF)
            for f in 0..2 {
                let fat_sec = 32 + f * 32;
                let fat_offset = (fat_sec * 512) as usize;
                // Entry 0: Media type (0x0FFFFF00 | 0xF8)
                let b0 = 0x0FFF_FFF8u32.to_le_bytes();
                core::ptr::copy_nonoverlapping(b0.as_ptr(), disk_ptr.add(fat_offset), 4);
                // Entry 1: EOC
                let b1 = 0x0FFF_FFFFu32.to_le_bytes();
                core::ptr::copy_nonoverlapping(b1.as_ptr(), disk_ptr.add(fat_offset + 4), 4);
                // Entry 2: Root directory EOC
                let b2 = 0x0FFF_FFFFu32.to_le_bytes();
                core::ptr::copy_nonoverlapping(b2.as_ptr(), disk_ptr.add(fat_offset + 8), 4);
            }

            let mut scratch = [0u8; 4096];
            let scratch_va = scratch.as_mut_ptr() as u64;
            let mut read_buf = [0u8; 4096];
            let read_va = read_buf.as_mut_ptr() as u64;

            // 1. Verify find_free_cluster finds cluster 3
            let free_clus = vol.find_free_cluster(&mut dev, 2, scratch_va, scratch_va).expect("find_free");
            assert_eq!(free_clus, 3);

            // 2. Write file TEST.TXT
            let payload = b"hello genesis stage 15";
            vol.write_file(&mut dev, "TEST.TXT", payload, scratch_va, scratch_va)
                .expect("write_file TEST.TXT failed");

            // 3. Read back TEST.TXT and assert exact match
            let read_len = vol.read_file(&mut dev, "TEST.TXT", read_va, read_va, 4096, scratch_va, scratch_va)
                .expect("read_file TEST.TXT failed");
            assert_eq!(read_len, payload.len());
            assert_eq!(&read_buf[..read_len], payload);

            // 4. Overwrite TEST.TXT with different payload
            let updated = b"overwritten genesis content!";
            vol.write_file(&mut dev, "TEST.TXT", updated, scratch_va, scratch_va)
                .expect("overwrite TEST.TXT failed");

            let read_len2 = vol.read_file(&mut dev, "TEST.TXT", read_va, read_va, 4096, scratch_va, scratch_va)
                .expect("read_file updated TEST.TXT failed");
            assert_eq!(read_len2, updated.len());
            assert_eq!(&read_buf[..read_len2], updated);

            // 5. Verify list_dir enumerates TEST.TXT
            let mut found = false;
            let mut found_size = 0usize;
            vol.list_dir(&mut dev, "/", scratch_va, scratch_va, |e| {
                if e.name_str() == "TEST.TXT" {
                    found = true;
                    found_size = e.size;
                }
            }).expect("list_dir failed");

            assert!(found, "TEST.TXT must be found in directory listing");
            assert_eq!(found_size, updated.len());
        }
    }

    #[test]
    fn test_lfn_checksum_calculation() {
        let short = *b"TEST    TXT";
        let chk = lfn_checksum(&short);
        let mut manual_sum: u8 = 0;
        for &b in short.iter() {
            manual_sum = (((manual_sum & 1) << 7) | (manual_sum >> 1)).wrapping_add(b);
        }
        assert_eq!(chk, manual_sum);
        assert_ne!(chk, 0);

        let short2 = *b"FOO     BAR";
        let mut s2: u8 = 0;
        for &b in short2.iter() {
            s2 = (((s2 & 1) << 7) | (s2 >> 1)).wrapping_add(b);
        }
        assert_eq!(lfn_checksum(&short2), s2);
    }

    #[test]
    fn test_lfn_entry_encode_and_decode() {
        let name_ucs2: [u16; 13] = [
            'H' as u16, 'e' as u16, 'l' as u16, 'l' as u16, 'o' as u16,
            'W' as u16, 'o' as u16, 'r' as u16, 'l' as u16, 'd' as u16,
            '.' as u16, 't' as u16, 'x' as u16,
        ];
        let chk = 0x42;
        let entry = build_lfn_entry(1, true, chk, &name_ucs2);

        // Verify sequence byte has sequence 1 and last flag 0x40
        assert_eq!(entry[0], 0x41);
        // Verify attribute is 0x0F
        assert_eq!(entry[11], 0x0F);
        // Verify checksum
        assert_eq!(entry[13], chk);

        // Decode back
        let mut extracted = [0u16; 13];
        extract_lfn_chars(&entry, &mut extracted);
        assert_eq!(&extracted, &name_ucs2);

        // Test LfnAccumulator
        let mut accum = LfnAccumulator::new();
        accum.process_lfn_slot(&entry);
        assert!(accum.valid);
        assert_eq!(accum.slot_count, 1);
        let mut out = [0u8; 64];
        let utf8_len = accum.to_utf8(&mut out);
        assert_eq!(core::str::from_utf8(&out[..utf8_len]), Ok("HelloWorld.tx"));
    }

    #[test]
    fn test_fat32_lfn_mkdir_rm_cp_mv() {
        unsafe {
            let mut dev = RamDisk::new(2 * 1024 * 1024);
            let disk_ptr = dev.data.as_mut_ptr();

            let vol = FatVolume {
                volume_lba: 0,
                bytes_per_sec: 512,
                sec_per_clus: 1,
                rsvd_sec_cnt: 32,
                num_fats: 2,
                fat_size: 32,
                root_clus: 2,
                fat_start_lba: 32,
                data_start_lba: 96,
                is_fat32: true,
                root_dir_lba: 0,
                root_dir_sectors: 0,
            };

            for f in 0..2 {
                let fat_sec = 32 + f * 32;
                let fat_offset = (fat_sec * 512) as usize;
                let b0 = 0x0FFF_FFF8u32.to_le_bytes();
                core::ptr::copy_nonoverlapping(b0.as_ptr(), disk_ptr.add(fat_offset), 4);
                let b1 = 0x0FFF_FFFFu32.to_le_bytes();
                core::ptr::copy_nonoverlapping(b1.as_ptr(), disk_ptr.add(fat_offset + 4), 4);
                let b2 = 0x0FFF_FFFFu32.to_le_bytes();
                core::ptr::copy_nonoverlapping(b2.as_ptr(), disk_ptr.add(fat_offset + 8), 4);
            }

            let mut scratch = [0u8; 4096];
            let scratch_va = scratch.as_mut_ptr() as u64;
            let mut buf = [0u8; 4096];
            let buf_va = buf.as_mut_ptr() as u64;

            // 1. mkdir TESTDIR
            vol.mkdir(&mut dev, "TESTDIR", scratch_va, scratch_va)
                .expect("mkdir TESTDIR failed");

            // Verify TESTDIR exists in root dir
            let (dir_clus, _, is_dir) = vol.find_entry_in_dir(&mut dev, vol.root_clus, "TESTDIR", scratch_va, scratch_va)
                .expect("find TESTDIR failed");
            assert!(is_dir);
            assert!(dir_clus >= 3);

            // 2. Write file inside TESTDIR with a long filename (LFN)
            let lfn_name = "LONGFILENAME.TXT";
            let lfn_path = "TESTDIR/LONGFILENAME.TXT";
            let payload = b"Hello from Genesis LFN FAT32 implementation!";
            vol.write_file(&mut dev, lfn_path, payload, scratch_va, scratch_va)
                .expect("write_file LFN failed");

            // Read it back
            let read_len = vol.read_file(&mut dev, lfn_path, buf_va, buf_va, 4096, scratch_va, scratch_va)
                .expect("read_file LFN failed");
            assert_eq!(read_len, payload.len());
            assert_eq!(&buf[..read_len], payload);

            // Check list_dir inside TESTDIR shows LONGFILENAME.TXT
            let mut found_lfn = false;
            vol.list_dir(&mut dev, "TESTDIR", scratch_va, scratch_va, |e| {
                if e.name_str() == lfn_name {
                    found_lfn = true;
                    assert_eq!(e.size, payload.len());
                    assert!(!e.is_dir);
                }
            }).expect("list_dir TESTDIR failed");
            assert!(found_lfn, "LONGFILENAME.TXT must appear in directory listing");

            // 3. Copy file: TESTDIR/LONGFILENAME.TXT -> TESTDIR/COPY.TXT
            let copy_path = "TESTDIR/COPY.TXT";
            let copied_bytes = vol.copy_file(&mut dev, lfn_path, copy_path, buf_va, buf_va, 4096, scratch_va, scratch_va)
                .expect("copy_file failed");
            assert_eq!(copied_bytes, payload.len());

            // Verify copy contents
            let copy_read = vol.read_file(&mut dev, copy_path, buf_va, buf_va, 4096, scratch_va, scratch_va)
                .expect("read_file copy failed");
            assert_eq!(copy_read, payload.len());
            assert_eq!(&buf[..copy_read], payload);

            // 4. Move file: TESTDIR/COPY.TXT -> TESTDIR/MOVED.TXT
            let moved_path = "TESTDIR/MOVED.TXT";
            vol.move_file(&mut dev, copy_path, moved_path, buf_va, buf_va, 4096, scratch_va, scratch_va)
                .expect("move_file failed");

            // Old path must no longer exist
            assert!(vol.read_file(&mut dev, copy_path, buf_va, buf_va, 4096, scratch_va, scratch_va).is_err());

            // New path must exist with payload
            let moved_read = vol.read_file(&mut dev, moved_path, buf_va, buf_va, 4096, scratch_va, scratch_va)
                .expect("read_file moved failed");
            assert_eq!(moved_read, payload.len());
            assert_eq!(&buf[..moved_read], payload);

            // 5. Remove files and directory
            vol.remove(&mut dev, moved_path, scratch_va, scratch_va)
                .expect("remove moved failed");
            assert!(vol.read_file(&mut dev, moved_path, buf_va, buf_va, 4096, scratch_va, scratch_va).is_err());

            // Directory cannot be removed while not empty
            assert!(vol.remove(&mut dev, "TESTDIR", scratch_va, scratch_va).is_err());

            // Remove LONGFILENAME.TXT
            vol.remove(&mut dev, lfn_path, scratch_va, scratch_va)
                .expect("remove lfn file failed");

            // Directory can now be removed
            vol.remove(&mut dev, "TESTDIR", scratch_va, scratch_va)
                .expect("remove TESTDIR failed");

            // TESTDIR no longer exists in root
            assert!(vol.find_entry_in_dir(&mut dev, vol.root_clus, "TESTDIR", scratch_va, scratch_va).is_err());
        }
    }
}

//! Project Genesis — File Systems Subsystem.

pub mod block;
pub mod fat32;

pub use block::{get_block_device_by_name, AnyBlockDev, BlockDevice};
pub use fat32::{locate_volume, to_8_3, FatDirEntry, FatVolume};

pub static mut ACTIVE_VOLUME: Option<FatVolume> = None;
pub static mut ACTIVE_VOLUME_DEV: [u8; 16] = [0; 16];
pub static mut ACTIVE_VOLUME_DEV_LEN: usize = 0;

pub fn set_active_volume(vol: FatVolume, dev_name: &str) {
    unsafe {
        ACTIVE_VOLUME = Some(vol);
        let b = dev_name.as_bytes();
        let len = b.len().min(16);
        ACTIVE_VOLUME_DEV[..len].copy_from_slice(&b[..len]);
        ACTIVE_VOLUME_DEV_LEN = len;
    }
}

#[allow(static_mut_refs)]
pub fn get_active_volume() -> Option<&'static FatVolume> {
    unsafe { ACTIVE_VOLUME.as_ref() }
}

#[allow(static_mut_refs)]
pub fn get_active_volume_name() -> &'static str {
    unsafe {
        if ACTIVE_VOLUME_DEV_LEN > 0 {
            core::str::from_utf8(&ACTIVE_VOLUME_DEV[..ACTIVE_VOLUME_DEV_LEN]).unwrap_or("none")
        } else {
            "none"
        }
    }
}

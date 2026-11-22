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

pub unsafe fn resolve_active_fs<'b>(
    fallback_fat: Option<&'b FatVolume>,
    fallback_dev: Option<&'b mut crate::drivers::VirtIOBlockDevice>,
) -> (Option<&'b FatVolume>, Option<AnyBlockDev<'b>>) {
    let active_name = get_active_volume_name();
    let active_fat = get_active_volume();
    if active_name != "none" && active_fat.is_some() {
        let f = active_fat.unwrap();
        if active_name == "blk0" && fallback_dev.is_some() {
            (Some(f), fallback_dev.map(AnyBlockDev::VirtIO))
        } else if let Some(bd) = get_block_device_by_name(active_name) {
            (Some(f), Some(bd))
        } else {
            (fallback_fat, fallback_dev.map(AnyBlockDev::VirtIO))
        }
    } else {
        (fallback_fat, fallback_dev.map(AnyBlockDev::VirtIO))
    }
}

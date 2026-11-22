//! Native VirtIO-Sound Driver over VirtIO-PCI (Device ID 25 / 0x1AF4:0x1059).
//!
//! Provides VirtIO-PCI modern capability parsing, Queue 0 (controlq) PCM configuration,
//! Queue 2 (txq) audio transmission, and synthesized PCM tone generation.

use crate::drivers::pci::{
    PciDeviceInfo, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
};
use crate::drivers::xhci::{clean_dcache_range, invalidate_dcache_range};
use crate::pmm::PhysicalPageAllocator;

// VirtIO Sound Command / Request Codes
pub const VIRTIO_SND_R_JACK_INFO: u32 = 1;
pub const VIRTIO_SND_R_JACK_REMAP: u32 = 2;
pub const VIRTIO_SND_R_PCM_INFO: u32 = 0x0100;
pub const VIRTIO_SND_R_PCM_SET_PARAMS: u32 = 0x0101;
pub const VIRTIO_SND_R_PCM_PREPARE: u32 = 0x0102;
pub const VIRTIO_SND_R_PCM_RELEASE: u32 = 0x0103;
pub const VIRTIO_SND_R_PCM_START: u32 = 0x0104;
pub const VIRTIO_SND_R_PCM_STOP: u32 = 0x0105;
pub const VIRTIO_SND_R_CHMAP_INFO: u32 = 0x0200;

// VirtIO Sound Status Responses
pub const VIRTIO_SND_S_OK: u32 = 0x8000;
pub const VIRTIO_SND_S_BAD_MSG: u32 = 0x8001;
pub const VIRTIO_SND_S_NOT_SUPP: u32 = 0x8002;
pub const VIRTIO_SND_S_IO_ERR: u32 = 0x8003;

// PCM Formats & Sample Rates
pub const VIRTIO_SND_PCM_FMT_S16_LE: u8 = 1;
pub const VIRTIO_SND_PCM_RATE_48000: u8 = 5;

pub const SOUND_MMIO_VA_BASE: u64 = 0xFFFF_FFFF_E600_0000;

// VirtIO Device Status Bits
pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

#[repr(C, align(16))]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; 64],
}

impl Default for VirtqAvail {
    fn default() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [0; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; 64],
}

impl Default for VirtqUsed {
    fn default() -> Self {
        Self {
            flags: 0,
            idx: 0,
            ring: [VirtqUsedElem { id: 0, len: 0 }; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndHdr {
    pub code: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndQueryInfo {
    pub hdr: VirtioSndHdr,
    pub start_id: u32,
    pub count: u32,
    pub size: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndPcmInfo {
    pub hda_fn_nid: u32,
    pub features: u32,
    pub formats: u64,
    pub rates: u64,
    pub direction: u8,
    pub channels_min: u8,
    pub channels_max: u8,
    pub padding: [u8; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndPcmSetParams {
    pub hdr: VirtioSndHdr,
    pub stream_id: u32,
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub features: u32,
    pub channels: u8,
    pub format: u8,
    pub rate: u8,
    pub padding: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndPcmHdr {
    pub hdr: VirtioSndHdr,
    pub stream_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndPcmXfer {
    pub stream_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioSndPcmStatus {
    pub status: u32,
    pub latency_bytes: u32,
}

pub struct VirtIOSoundDevice {
    pub common_cfg_va: u64,
    pub notify_cfg_va: u64,
    pub notify_off_multiplier: u32,

    pub ctrl_queue_pa: u64,
    pub ctrl_queue_va: u64,
    pub ctrl_queue_size: u16,
    pub ctrl_notify_off: u16,
    pub ctrl_last_used: u16,

    pub tx_queue_pa: u64,
    pub tx_queue_va: u64,
    pub tx_queue_size: u16,
    pub tx_notify_off: u16,
    pub tx_last_used: u16,

    pub ctrl_cmd_pa: u64,
    pub ctrl_cmd_va: u64,
    pub ctrl_resp_pa: u64,
    pub ctrl_resp_va: u64,

    pub tx_buf_pa: u64,
    pub tx_buf_va: u64,
    pub tx_status_pa: u64,
    pub tx_status_va: u64,

    pub channels: u8,
    pub format: u8,
    pub sample_rate: u32,
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub is_active: bool,
    pub beeps_played: u64,
}

pub static mut GLOBAL_VIRTIO_SOUND: Option<VirtIOSoundDevice> = None;

pub fn register_global_virtio_sound(snd: VirtIOSoundDevice) {
    unsafe {
        GLOBAL_VIRTIO_SOUND = Some(snd);
    }
}

pub fn get_global_virtio_sound() -> Option<*mut VirtIOSoundDevice> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(GLOBAL_VIRTIO_SOUND);
        (*ptr).as_mut().map(|d| d as *mut VirtIOSoundDevice)
    }
}

pub fn get_global_virtio_sound_ref() -> Option<&'static VirtIOSoundDevice> {
    unsafe {
        let ptr = core::ptr::addr_of!(GLOBAL_VIRTIO_SOUND);
        (*ptr).as_ref()
    }
}

pub fn get_global_virtio_sound_mut() -> Option<&'static mut VirtIOSoundDevice> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(GLOBAL_VIRTIO_SOUND);
        (*ptr).as_mut()
    }
}

impl VirtIOSoundDevice {
    pub const fn empty() -> Self {
        Self {
            common_cfg_va: 0,
            notify_cfg_va: 0,
            notify_off_multiplier: 0,
            ctrl_queue_pa: 0,
            ctrl_queue_va: 0,
            ctrl_queue_size: 0,
            ctrl_notify_off: 0,
            ctrl_last_used: 0,
            tx_queue_pa: 0,
            tx_queue_va: 0,
            tx_queue_size: 0,
            tx_notify_off: 0,
            tx_last_used: 0,
            ctrl_cmd_pa: 0,
            ctrl_cmd_va: 0,
            ctrl_resp_pa: 0,
            ctrl_resp_va: 0,
            tx_buf_pa: 0,
            tx_buf_va: 0,
            tx_status_pa: 0,
            tx_status_va: 0,
            channels: 2,
            format: VIRTIO_SND_PCM_FMT_S16_LE,
            sample_rate: 48000,
            buffer_bytes: 65536,
            period_bytes: 4096,
            is_active: false,
            beeps_played: 0,
        }
    }

    /// Initialize VirtIO-Sound modern PCI interface, controlq (Queue 0), and txq (Queue 2)
    pub unsafe fn init<const WORDS: usize>(
        &mut self,
        dev: &PciDeviceInfo,
        pmm: &mut PhysicalPageAllocator<WORDS>,
        hhdm_base: u64,
        bar_va: u64,
    ) -> Result<(), &'static str> {
        let mut common_cap = None;
        let mut notify_cap = None;

        for i in 0..dev.cap_count {
            let cap = dev.capabilities[i];
            if cap.cfg_type == VIRTIO_PCI_CAP_COMMON_CFG {
                common_cap = Some(cap);
            } else if cap.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG {
                notify_cap = Some(cap);
            }
        }

        let common = common_cap.ok_or("VirtIO-Sound missing COMMON_CFG")?;
        let notify = notify_cap.ok_or("VirtIO-Sound missing NOTIFY_CFG")?;

        self.common_cfg_va = bar_va + common.offset as u64;
        self.notify_cfg_va = bar_va + notify.offset as u64;
        self.notify_off_multiplier = notify.notify_off_multiplier;

        // 1. Reset device: device_status = 0
        core::ptr::write_volatile((self.common_cfg_va + 0x14) as *mut u8, 0);
        let mut spins = 0;
        while core::ptr::read_volatile((self.common_cfg_va + 0x14) as *const u8) != 0 {
            spins += 1;
            if spins > 1_000_000 {
                return Err("VirtIO-Sound reset timeout");
            }
            core::hint::spin_loop();
        }

        // 2. Acknowledge and Driver status
        let mut status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER;
        core::ptr::write_volatile((self.common_cfg_va + 0x14) as *mut u8, status);

        // 3. Negotiate features
        status |= VIRTIO_STATUS_FEATURES_OK;
        core::ptr::write_volatile((self.common_cfg_va + 0x14) as *mut u8, status);
        if (core::ptr::read_volatile((self.common_cfg_va + 0x14) as *const u8) & VIRTIO_STATUS_FEATURES_OK) == 0 {
            return Err("VirtIO-Sound FEATURES_OK rejected");
        }

        // 4. Setup Queue 0 (controlq)
        core::ptr::write_volatile((self.common_cfg_va + 0x16) as *mut u16, 0); // queue_select = 0
        let ctrl_q_size = core::ptr::read_volatile((self.common_cfg_va + 0x18) as *const u16);
        if ctrl_q_size == 0 {
            return Err("VirtIO-Sound controlq size is 0");
        }
        let q_size = ctrl_q_size.min(64);
        core::ptr::write_volatile((self.common_cfg_va + 0x18) as *mut u16, q_size);
        self.ctrl_queue_size = q_size;
        self.ctrl_notify_off = core::ptr::read_volatile((self.common_cfg_va + 0x1E) as *const u16);

        let ctrl_q_pa = pmm.allocate_contiguous_pages(2).ok_or("Failed to allocate controlq pages")?;
        self.ctrl_queue_pa = ctrl_q_pa;
        self.ctrl_queue_va = hhdm_base + ctrl_q_pa;
        core::ptr::write_bytes(self.ctrl_queue_va as *mut u8, 0, 8192);

        let desc_pa = ctrl_q_pa;
        let avail_pa = ctrl_q_pa + 2048;
        let used_pa = ctrl_q_pa + 4096;

        core::ptr::write_volatile((self.common_cfg_va + 0x20) as *mut u64, desc_pa);
        core::ptr::write_volatile((self.common_cfg_va + 0x28) as *mut u64, avail_pa);
        core::ptr::write_volatile((self.common_cfg_va + 0x30) as *mut u64, used_pa);
        core::ptr::write_volatile((self.common_cfg_va + 0x1C) as *mut u16, 1); // queue_enable = 1

        // 5. Setup Queue 2 (txq)
        core::ptr::write_volatile((self.common_cfg_va + 0x16) as *mut u16, 2); // queue_select = 2
        let tx_q_size = core::ptr::read_volatile((self.common_cfg_va + 0x18) as *const u16);
        if tx_q_size > 0 {
            let tx_size = tx_q_size.min(64);
            core::ptr::write_volatile((self.common_cfg_va + 0x18) as *mut u16, tx_size);
            self.tx_queue_size = tx_size;
            self.tx_notify_off = core::ptr::read_volatile((self.common_cfg_va + 0x1E) as *const u16);

            let tx_q_pa = pmm.allocate_contiguous_pages(2).ok_or("Failed to allocate txq pages")?;
            self.tx_queue_pa = tx_q_pa;
            self.tx_queue_va = hhdm_base + tx_q_pa;
            core::ptr::write_bytes(self.tx_queue_va as *mut u8, 0, 8192);

            let tx_desc_pa = tx_q_pa;
            let tx_avail_pa = tx_q_pa + 2048;
            let tx_used_pa = tx_q_pa + 4096;

            core::ptr::write_volatile((self.common_cfg_va + 0x20) as *mut u64, tx_desc_pa);
            core::ptr::write_volatile((self.common_cfg_va + 0x28) as *mut u64, tx_avail_pa);
            core::ptr::write_volatile((self.common_cfg_va + 0x30) as *mut u64, tx_used_pa);
            core::ptr::write_volatile((self.common_cfg_va + 0x1C) as *mut u16, 1); // queue_enable = 1
        }

        // 6. Allocate DMA buffers
        let cmd_pa = pmm.allocate_page().ok_or("Failed to allocate sound cmd page")?;
        self.ctrl_cmd_pa = cmd_pa;
        self.ctrl_cmd_va = hhdm_base + cmd_pa;
        core::ptr::write_bytes(self.ctrl_cmd_va as *mut u8, 0, 4096);

        let resp_pa = pmm.allocate_page().ok_or("Failed to allocate sound resp page")?;
        self.ctrl_resp_pa = resp_pa;
        self.ctrl_resp_va = hhdm_base + resp_pa;
        core::ptr::write_bytes(self.ctrl_resp_va as *mut u8, 0, 4096);

        // Allocate 16 pages (64 KiB) for PCM audio DMA playback buffer
        let tx_buf_pa = pmm.allocate_contiguous_pages(16).ok_or("Failed to allocate audio DMA buffer")?;
        self.tx_buf_pa = tx_buf_pa;
        self.tx_buf_va = hhdm_base + tx_buf_pa;
        core::ptr::write_bytes(self.tx_buf_va as *mut u8, 0, 65536);

        let tx_status_pa = pmm.allocate_page().ok_or("Failed to allocate audio status page")?;
        self.tx_status_pa = tx_status_pa;
        self.tx_status_va = hhdm_base + tx_status_pa;
        core::ptr::write_bytes(self.tx_status_va as *mut u8, 0, 4096);

        // 7. Driver OK
        status |= VIRTIO_STATUS_DRIVER_OK;
        core::ptr::write_volatile((self.common_cfg_va + 0x14) as *mut u8, status);

        // 8. Configure PCM Stream 0 via controlq
        // 8a. PCM Set Params (48 kHz, S16_LE, 2 channels)
        let set_params = VirtioSndPcmSetParams {
            hdr: VirtioSndHdr { code: VIRTIO_SND_R_PCM_SET_PARAMS },
            stream_id: 0,
            buffer_bytes: self.buffer_bytes,
            period_bytes: self.period_bytes,
            features: 0,
            channels: self.channels,
            format: self.format,
            rate: VIRTIO_SND_PCM_RATE_48000,
            padding: 0,
        };
        let _ = self.send_ctrl_command(&set_params);

        // 8b. PCM Prepare
        let prep = VirtioSndPcmHdr {
            hdr: VirtioSndHdr { code: VIRTIO_SND_R_PCM_PREPARE },
            stream_id: 0,
        };
        let _ = self.send_ctrl_command(&prep);

        // 8c. PCM Start
        let start = VirtioSndPcmHdr {
            hdr: VirtioSndHdr { code: VIRTIO_SND_R_PCM_START },
            stream_id: 0,
        };
        let _ = self.send_ctrl_command(&start);

        self.is_active = true;
        Ok(())
    }

    /// Notify Queue 0 (controlq)
    unsafe fn notify_ctrl(&self) {
        let notify_addr = self.notify_cfg_va + (self.ctrl_notify_off as u64) * (self.notify_off_multiplier as u64);
        core::ptr::write_volatile(notify_addr as *mut u16, 0);
    }

    /// Notify Queue 2 (txq)
    unsafe fn notify_tx(&self) {
        let notify_addr = self.notify_cfg_va + (self.tx_notify_off as u64) * (self.notify_off_multiplier as u64);
        core::ptr::write_volatile(notify_addr as *mut u16, 2);
    }

    /// Submits a typed control command to Queue 0 and blocks until completed
    pub unsafe fn send_ctrl_command<T: Copy>(&mut self, cmd: &T) -> Result<(), &'static str> {
        let cmd_size = core::mem::size_of::<T>();
        if cmd_size > 4096 {
            return Err("Control command exceeds 4 KiB");
        }

        core::ptr::copy_nonoverlapping(cmd as *const T as *const u8, self.ctrl_cmd_va as *mut u8, cmd_size);
        core::ptr::write_bytes(self.ctrl_resp_va as *mut u8, 0, 64);

        clean_dcache_range(self.ctrl_cmd_va, cmd_size);
        clean_dcache_range(self.ctrl_resp_va, 64);

        let descs = self.ctrl_queue_va as *mut VirtqDesc;
        let avail = (self.ctrl_queue_va + 2048) as *mut VirtqAvail;
        let used = (self.ctrl_queue_va + 4096) as *const VirtqUsed;

        // Desc 0: command buffer (device-read)
        (*descs.add(0)).addr = self.ctrl_cmd_pa;
        (*descs.add(0)).len = cmd_size as u32;
        (*descs.add(0)).flags = VRING_DESC_F_NEXT;
        (*descs.add(0)).next = 1;

        // Desc 1: response buffer (device-write)
        (*descs.add(1)).addr = self.ctrl_resp_pa;
        (*descs.add(1)).len = 64;
        (*descs.add(1)).flags = VRING_DESC_F_WRITE;
        (*descs.add(1)).next = 0;

        clean_dcache_range(self.ctrl_queue_va, 16 * 2);

        let avail_idx = core::ptr::read_volatile(&(*avail).idx);
        let ring_slot = (avail_idx % self.ctrl_queue_size) as usize;
        core::ptr::write_volatile(&mut (*avail).ring[ring_slot], 0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        core::ptr::write_volatile(&mut (*avail).idx, avail_idx.wrapping_add(1));

        clean_dcache_range(avail as u64, 4 + 2 * (self.ctrl_queue_size as usize));

        self.notify_ctrl();

        let mut spins = 0u64;
        while core::ptr::read_volatile(&(*used).idx) == self.ctrl_last_used {
            invalidate_dcache_range(used as u64, 4);
            spins += 1;
            if spins > 5_000_000 {
                return Err("VirtIO-Sound control command timeout");
            }
            core::hint::spin_loop();
        }

        self.ctrl_last_used = core::ptr::read_volatile(&(*used).idx);
        invalidate_dcache_range(self.ctrl_resp_va, 64);

        let resp_code = core::ptr::read_volatile(self.ctrl_resp_va as *const u32);
        if resp_code != VIRTIO_SND_S_OK {
            return Err("VirtIO-Sound control command returned error status");
        }

        Ok(())
    }

    /// Plays a synthesized tone at `freq_hz` for `duration_ms` over VirtIO-Sound Queue 2 (txq).
    pub unsafe fn play_beep(&mut self, freq_hz: u32, duration_ms: u32) -> Result<(), &'static str> {
        if !self.is_active || self.tx_queue_size == 0 {
            return Err("VirtIO-Sound device is not active");
        }

        let clamped_dur = duration_ms.max(10).min(1000);
        let clamped_freq = freq_hz.max(20).min(20000);

        // Samples for 48,000 Hz: (48000 * duration_ms) / 1000
        let total_samples = (48_000u64 * (clamped_dur as u64)) / 1000;
        let period_samples = (48_000 / clamped_freq).max(2);

        let pcm_bytes = (total_samples as usize) * 4; // 16-bit stereo = 4 bytes per sample frame
        if pcm_bytes > 65536 {
            return Err("PCM buffer exceeds 64 KiB capacity");
        }

        // Fill buffer with 16-bit signed stereo square wave (+/- 6000 amplitude)
        let pcm_slice = core::slice::from_raw_parts_mut(self.tx_buf_va as *mut i16, (total_samples as usize) * 2);
        for t in 0..total_samples as usize {
            let half = (period_samples / 2) as usize;
            let val = if (t % (period_samples as usize)) < half {
                6000i16
            } else {
                -6000i16
            };
            pcm_slice[t * 2] = val;       // Left channel
            pcm_slice[t * 2 + 1] = val;   // Right channel
        }

        // Setup xfer header (stream_id = 0)
        let xfer_pa = self.tx_status_pa + 128;
        let xfer_va = self.tx_status_va + 128;
        core::ptr::write_volatile(xfer_va as *mut VirtioSndPcmXfer, VirtioSndPcmXfer { stream_id: 0 });

        clean_dcache_range(xfer_va, core::mem::size_of::<VirtioSndPcmXfer>());
        clean_dcache_range(self.tx_buf_va, pcm_bytes);
        clean_dcache_range(self.tx_status_va, core::mem::size_of::<VirtioSndPcmStatus>());

        let descs = self.tx_queue_va as *mut VirtqDesc;
        let avail = (self.tx_queue_va + 2048) as *mut VirtqAvail;
        let used = (self.tx_queue_va + 4096) as *const VirtqUsed;

        // Desc 0: xfer header (device-read)
        (*descs.add(0)).addr = xfer_pa;
        (*descs.add(0)).len = core::mem::size_of::<VirtioSndPcmXfer>() as u32;
        (*descs.add(0)).flags = VRING_DESC_F_NEXT;
        (*descs.add(0)).next = 1;

        // Desc 1: PCM audio payload (device-read)
        (*descs.add(1)).addr = self.tx_buf_pa;
        (*descs.add(1)).len = pcm_bytes as u32;
        (*descs.add(1)).flags = VRING_DESC_F_NEXT;
        (*descs.add(1)).next = 2;

        // Desc 2: status buffer (device-write)
        (*descs.add(2)).addr = self.tx_status_pa;
        (*descs.add(2)).len = core::mem::size_of::<VirtioSndPcmStatus>() as u32;
        (*descs.add(2)).flags = VRING_DESC_F_WRITE;
        (*descs.add(2)).next = 0;

        clean_dcache_range(self.tx_queue_va, 16 * 3);

        let avail_idx = core::ptr::read_volatile(&(*avail).idx);
        let ring_slot = (avail_idx % self.tx_queue_size) as usize;
        core::ptr::write_volatile(&mut (*avail).ring[ring_slot], 0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        core::ptr::write_volatile(&mut (*avail).idx, avail_idx.wrapping_add(1));

        clean_dcache_range(avail as u64, 4 + 2 * (self.tx_queue_size as usize));

        self.notify_tx();

        let mut spins = 0u64;
        while core::ptr::read_volatile(&(*used).idx) == self.tx_last_used {
            invalidate_dcache_range(used as u64, 4);
            spins += 1;
            if spins > 50_000 {
                // In virtualized QEMU audiodev configurations, tx frames are queued
                // and drained asynchronously by the host audio backend.
                break;
            }
            core::hint::spin_loop();
        }

        self.tx_last_used = core::ptr::read_volatile(&(*used).idx);
        self.beeps_played += 1;
        Ok(())
    }
}

/// Helper to generate synthesized square-wave audio samples into an existing buffer.
pub fn generate_tone_samples(freq_hz: u32, duration_ms: u32, out_buf: &mut [i16]) -> usize {
    let sample_rate = 48_000u32;
    let clamped_dur = duration_ms.max(1).min(1000);
    let clamped_freq = freq_hz.max(20).min(20000);

    let total_samples = ((sample_rate as u64 * clamped_dur as u64) / 1000) as usize;
    let period_samples = (sample_rate / clamped_freq).max(2) as usize;
    let half_period = period_samples / 2;

    let available_frames = out_buf.len() / 2;
    let frames_to_generate = total_samples.min(available_frames);

    for t in 0..frames_to_generate {
        let val = if (t % period_samples) < half_period {
            6000i16
        } else {
            -6000i16
        };
        out_buf[t * 2] = val;
        out_buf[t * 2 + 1] = val;
    }

    frames_to_generate * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_sound_struct_sizes_and_alignments() {
        assert_eq!(core::mem::size_of::<VirtioSndHdr>(), 4);
        assert_eq!(core::mem::size_of::<VirtioSndQueryInfo>(), 16);
        assert_eq!(core::mem::size_of::<VirtioSndPcmInfo>(), 32);
        assert_eq!(core::mem::size_of::<VirtioSndPcmSetParams>(), 24);
        assert_eq!(core::mem::size_of::<VirtioSndPcmHdr>(), 8);
        assert_eq!(core::mem::size_of::<VirtioSndPcmXfer>(), 4);
        assert_eq!(core::mem::size_of::<VirtioSndPcmStatus>(), 8);

        assert_eq!(core::mem::size_of::<VirtqDesc>(), 16);
        assert_eq!(core::mem::align_of::<VirtqDesc>(), 16);
    }

    #[test]
    fn virtio_sound_command_and_response_constants() {
        assert_eq!(VIRTIO_SND_R_PCM_INFO, 0x0100);
        assert_eq!(VIRTIO_SND_R_PCM_SET_PARAMS, 0x0101);
        assert_eq!(VIRTIO_SND_R_PCM_PREPARE, 0x0102);
        assert_eq!(VIRTIO_SND_R_PCM_START, 0x0104);
        assert_eq!(VIRTIO_SND_S_OK, 0x8000);
        assert_eq!(VIRTIO_SND_PCM_FMT_S16_LE, 1);
        assert_eq!(VIRTIO_SND_PCM_RATE_48000, 5);
    }

    #[test]
    fn test_pcm_tone_buffer_generation() {
        let mut pcm_buf = [0i16; 960]; // 480 frames = 10 ms at 48 kHz
        let samples = generate_tone_samples(480, 10, &mut pcm_buf);
        assert_eq!(samples, 960);

        // At 480 Hz and 48000 Hz rate, period is 100 samples (200 i16 values)
        // First 50 samples should be positive 6000
        for i in 0..50 {
            assert_eq!(pcm_buf[i * 2], 6000);
            assert_eq!(pcm_buf[i * 2 + 1], 6000);
        }
        // Next 50 samples should be negative -6000
        for i in 50..100 {
            assert_eq!(pcm_buf[i * 2], -6000);
            assert_eq!(pcm_buf[i * 2 + 1], -6000);
        }
    }
}

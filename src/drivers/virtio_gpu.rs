//! Native VirtIO-GPU 2D Driver over VirtIO-PCI.

use crate::drivers::pci::{PciDeviceInfo, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};
use crate::pmm::PhysicalPageAllocator;

pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;

pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1102;
pub const VIRTIO_GPU_RESP_OK_CAPSET: u32 = 0x1103;
pub const VIRTIO_GPU_RESP_OK_EDID: u32 = 0x1104;

pub const VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM: u32 = 1;
pub const VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: u32 = 2;
pub const VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM: u32 = 3;
pub const VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM: u32 = 4;
pub const VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM: u32 = 67;
pub const VIRTIO_GPU_FORMAT_X8B8G8R8_UNORM: u32 = 68;

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
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioGpuCtrlHdr {
    pub hdr_type: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioGpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuResourceCreate2d {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuMemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuResourceAttachBacking {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub nr_entries: u32,
    pub entries: [VirtioGpuMemEntry; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuSetScanout {
    pub hdr: VirtioGpuCtrlHdr,
    pub r: VirtioGpuRect,
    pub scanout_id: u32,
    pub resource_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuTransferToHost2d {
    pub hdr: VirtioGpuCtrlHdr,
    pub r: VirtioGpuRect,
    pub offset: u64,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuResourceFlush {
    pub hdr: VirtioGpuCtrlHdr,
    pub r: VirtioGpuRect,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuDisplayOne {
    pub r: VirtioGpuRect,
    pub enabled: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VirtioGpuRespDisplayInfo {
    pub hdr: VirtioGpuCtrlHdr,
    pub pmodes: [VirtioGpuDisplayOne; 16],
}

pub struct VirtIOGpuDevice {
    pub common_cfg_va: u64,
    pub notify_cfg_va: u64,
    pub notify_off_multiplier: u32,
    pub notify_off: u16,
    pub queue_size: u16,
    pub queue_pa: u64,
    pub queue_va: u64,
    pub cmd_buf_pa: u64,
    pub cmd_buf_va: u64,
    pub resp_buf_pa: u64,
    pub resp_buf_va: u64,
    pub width: u32,
    pub height: u32,
    pub resource_id: u32,
    pub last_used_idx: u16,
    pub is_initialized: bool,
}

pub static mut GLOBAL_VIRTIO_GPU: Option<*mut VirtIOGpuDevice> = None;

pub fn register_global_virtio_gpu(gpu: *mut VirtIOGpuDevice) {
    unsafe {
        GLOBAL_VIRTIO_GPU = Some(gpu);
    }
}

pub fn get_global_virtio_gpu() -> Option<*mut VirtIOGpuDevice> {
    unsafe { GLOBAL_VIRTIO_GPU }
}

impl VirtIOGpuDevice {
    pub const fn empty() -> Self {
        Self {
            common_cfg_va: 0,
            notify_cfg_va: 0,
            notify_off_multiplier: 0,
            notify_off: 0,
            queue_size: 0,
            queue_pa: 0,
            queue_va: 0,
            cmd_buf_pa: 0,
            cmd_buf_va: 0,
            resp_buf_pa: 0,
            resp_buf_va: 0,
            width: 0,
            height: 0,
            resource_id: 1,
            last_used_idx: 0,
            is_initialized: false,
        }
    }

    /// Initialize VirtIO-GPU modern PCI interface and Queue 0
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

        let common = common_cap.ok_or("VirtIO Common Cfg capability missing")?;
        let notify = notify_cap.ok_or("VirtIO Notify Cfg capability missing")?;

        self.common_cfg_va = bar_va + common.offset as u64;
        self.notify_cfg_va = bar_va + notify.offset as u64;
        self.notify_off_multiplier = notify.notify_off_multiplier;

        // Reset device
        core::ptr::write_volatile((self.common_cfg_va + 0x14) as *mut u8, 0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Set ACKNOWLEDGE | DRIVER
        core::ptr::write_volatile(
            (self.common_cfg_va + 0x14) as *mut u8,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
        );
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Select Queue 0 (controlq)
        core::ptr::write_volatile((self.common_cfg_va + 0x16) as *mut u16, 0);
        let q_size = core::ptr::read_volatile((self.common_cfg_va + 0x18) as *const u16);
        if q_size == 0 {
            return Err("VirtIO-GPU controlq size is 0");
        }
        let q_size = q_size.min(64);
        core::ptr::write_volatile((self.common_cfg_va + 0x18) as *mut u16, q_size);
        self.queue_size = q_size;

        self.notify_off = core::ptr::read_volatile((self.common_cfg_va + 0x1E) as *const u16);

        // Allocate 2 contiguous pages for VirtQueue buffers
        let q_pa = pmm.allocate_contiguous_pages(2).ok_or("Failed to allocate GPU virtqueue")?;
        let q_va = hhdm_base + q_pa;
        core::ptr::write_bytes(q_va as *mut u8, 0, 8192);

        self.queue_pa = q_pa;
        self.queue_va = q_va;

        // Descriptor table: 0..16*q_size (e.g. 16*64 = 1024 bytes)
        // Avail ring: offset 2048
        // Used ring: offset 4096 (second page)
        let desc_pa = q_pa;
        let avail_pa = q_pa + 2048;
        let used_pa = q_pa + 4096;

        core::ptr::write_volatile((self.common_cfg_va + 0x20) as *mut u64, desc_pa);
        core::ptr::write_volatile((self.common_cfg_va + 0x28) as *mut u64, avail_pa);
        core::ptr::write_volatile((self.common_cfg_va + 0x30) as *mut u64, used_pa);

        // Enable Queue 0
        core::ptr::write_volatile((self.common_cfg_va + 0x1C) as *mut u16, 1);

        // Set DRIVER_OK
        let status = core::ptr::read_volatile((self.common_cfg_va + 0x14) as *const u8);
        core::ptr::write_volatile(
            (self.common_cfg_va + 0x14) as *mut u8,
            status | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK,
        );
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Allocate 1 page for command & response buffers
        let buf_pa = pmm.allocate_page().ok_or("Failed to allocate GPU cmd buffer")?;
        let buf_va = hhdm_base + buf_pa;
        core::ptr::write_bytes(buf_va as *mut u8, 0, 4096);

        self.cmd_buf_pa = buf_pa;
        self.cmd_buf_va = buf_va;
        self.resp_buf_pa = buf_pa + 2048;
        self.resp_buf_va = buf_va + 2048;
        self.last_used_idx = 0;
        self.is_initialized = true;

        Ok(())
    }

    /// Notify device on controlq
    unsafe fn notify(&self) {
        let notify_addr = self.notify_cfg_va + (self.notify_off as u64 * self.notify_off_multiplier as u64);
        core::ptr::write_volatile(notify_addr as *mut u16, 0);
    }

    /// Submit a command from `cmd_buf_pa` and wait for response in `resp_buf_pa`
    pub unsafe fn send_ctrl_command(&mut self, req_len: u32, resp_len: u32) -> Result<(), &'static str> {
        if !self.is_initialized {
            return Err("GPU not initialized");
        }

        let desc_table = self.queue_va as *mut VirtqDesc;
        let avail = (self.queue_va + 2048) as *mut VirtqAvail<64>;
        let used = (self.queue_va + 4096) as *const VirtqUsed<64>;

        // Desc 0: Request buffer (read by device)
        core::ptr::write_volatile(
            desc_table.add(0),
            VirtqDesc {
                addr: self.cmd_buf_pa,
                len: req_len,
                flags: VRING_DESC_F_NEXT,
                next: 1,
            },
        );

        // Desc 1: Response buffer (written by device)
        core::ptr::write_volatile(
            desc_table.add(1),
            VirtqDesc {
                addr: self.resp_buf_pa,
                len: resp_len,
                flags: VRING_DESC_F_WRITE,
                next: 0,
            },
        );

        // Add head descriptor (0) to Avail ring
        let avail_idx = core::ptr::read_volatile(&(*avail).idx);
        let ring_slot = (avail_idx as usize) % (self.queue_size as usize);
        core::ptr::write_volatile(&mut (*avail).ring[ring_slot], 0);

        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        core::ptr::write_volatile(&mut (*avail).idx, avail_idx.wrapping_add(1));

        // Flush request buffer to memory & notify
        #[cfg(target_arch = "aarch64")]
        {
            core::arch::asm!("dsb sy", options(nomem, nostack));
        }
        self.notify();

        // Poll used ring until device marks descriptor done
        let target_idx = self.last_used_idx.wrapping_add(1);
        let mut spins = 0u32;
        loop {
            let cur_idx = core::ptr::read_volatile(&(*used).idx);
            if cur_idx == target_idx {
                self.last_used_idx = cur_idx;
                break;
            }
            spins += 1;
            if spins > 5_000_000 {
                return Err("VirtIO-GPU command timed out");
            }
            core::hint::spin_loop();
        }

        // Invalidate response buffer
        #[cfg(target_arch = "aarch64")]
        {
            core::arch::asm!("dsb sy", options(nomem, nostack));
        }

        let resp_hdr = core::ptr::read_volatile(self.resp_buf_va as *const VirtioGpuCtrlHdr);
        if resp_hdr.hdr_type != VIRTIO_GPU_RESP_OK_NODATA && resp_hdr.hdr_type != VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
            return Err("VirtIO-GPU response error");
        }

        Ok(())
    }

    /// Set up 2D scanout resource attached to backing physical buffer
    pub unsafe fn init_2d_scanout(
        &mut self,
        width: u32,
        height: u32,
        backing_pa: u64,
    ) -> Result<(), &'static str> {
        self.width = width;
        self.height = height;
        self.resource_id = 1;

        // 1. RESOURCE_CREATE_2D
        let create_cmd = self.cmd_buf_va as *mut VirtioGpuResourceCreate2d;
        core::ptr::write_volatile(
            create_cmd,
            VirtioGpuResourceCreate2d {
                hdr: VirtioGpuCtrlHdr {
                    hdr_type: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0,
                },
                resource_id: self.resource_id,
                format: VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
                width,
                height,
            },
        );
        self.send_ctrl_command(
            core::mem::size_of::<VirtioGpuResourceCreate2d>() as u32,
            core::mem::size_of::<VirtioGpuCtrlHdr>() as u32,
        )?;

        // 2. RESOURCE_ATTACH_BACKING
        let attach_cmd = self.cmd_buf_va as *mut VirtioGpuResourceAttachBacking;
        core::ptr::write_volatile(
            attach_cmd,
            VirtioGpuResourceAttachBacking {
                hdr: VirtioGpuCtrlHdr {
                    hdr_type: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0,
                },
                resource_id: self.resource_id,
                nr_entries: 1,
                entries: [VirtioGpuMemEntry {
                    addr: backing_pa,
                    length: width * height * 4,
                    padding: 0,
                }],
            },
        );
        self.send_ctrl_command(
            core::mem::size_of::<VirtioGpuResourceAttachBacking>() as u32,
            core::mem::size_of::<VirtioGpuCtrlHdr>() as u32,
        )?;

        // 3. SET_SCANOUT (Scanout 0, Resource 1)
        let scanout_cmd = self.cmd_buf_va as *mut VirtioGpuSetScanout;
        core::ptr::write_volatile(
            scanout_cmd,
            VirtioGpuSetScanout {
                hdr: VirtioGpuCtrlHdr {
                    hdr_type: VIRTIO_GPU_CMD_SET_SCANOUT,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0,
                },
                r: VirtioGpuRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                scanout_id: 0,
                resource_id: self.resource_id,
            },
        );
        self.send_ctrl_command(
            core::mem::size_of::<VirtioGpuSetScanout>() as u32,
            core::mem::size_of::<VirtioGpuCtrlHdr>() as u32,
        )?;

        Ok(())
    }

    /// Transfer updated framebuffer rectangular region to host and flush to display
    pub unsafe fn flush_rect(&mut self, x: u32, y: u32, width: u32, height: u32) -> Result<(), &'static str> {
        // 1. TRANSFER_TO_HOST_2D
        let xfer_cmd = self.cmd_buf_va as *mut VirtioGpuTransferToHost2d;
        core::ptr::write_volatile(
            xfer_cmd,
            VirtioGpuTransferToHost2d {
                hdr: VirtioGpuCtrlHdr {
                    hdr_type: VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0,
                },
                r: VirtioGpuRect {
                    x,
                    y,
                    width,
                    height,
                },
                offset: 0,
                resource_id: self.resource_id,
                padding: 0,
            },
        );
        self.send_ctrl_command(
            core::mem::size_of::<VirtioGpuTransferToHost2d>() as u32,
            core::mem::size_of::<VirtioGpuCtrlHdr>() as u32,
        )?;

        // 2. RESOURCE_FLUSH
        let flush_cmd = self.cmd_buf_va as *mut VirtioGpuResourceFlush;
        core::ptr::write_volatile(
            flush_cmd,
            VirtioGpuResourceFlush {
                hdr: VirtioGpuCtrlHdr {
                    hdr_type: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0,
                },
                r: VirtioGpuRect {
                    x,
                    y,
                    width,
                    height,
                },
                resource_id: self.resource_id,
                padding: 0,
            },
        );
        self.send_ctrl_command(
            core::mem::size_of::<VirtioGpuResourceFlush>() as u32,
            core::mem::size_of::<VirtioGpuCtrlHdr>() as u32,
        )?;

        Ok(())
    }

    /// Flush full screen
    pub unsafe fn flush(&mut self) -> Result<(), &'static str> {
        let (w, h) = (self.width, self.height);
        self.flush_rect(0, 0, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_gpu_struct_sizes_and_alignments() {
        assert_eq!(core::mem::size_of::<VirtioGpuCtrlHdr>(), 24);
        assert_eq!(core::mem::size_of::<VirtioGpuRect>(), 16);
        assert_eq!(core::mem::size_of::<VirtioGpuResourceCreate2d>(), 40);
        assert_eq!(core::mem::size_of::<VirtioGpuMemEntry>(), 16);
        assert_eq!(core::mem::size_of::<VirtioGpuResourceAttachBacking>(), 48);
        assert_eq!(core::mem::size_of::<VirtioGpuSetScanout>(), 48);
        assert_eq!(core::mem::size_of::<VirtioGpuTransferToHost2d>(), 56);
        assert_eq!(core::mem::size_of::<VirtioGpuResourceFlush>(), 48);
        assert_eq!(core::mem::size_of::<VirtioGpuDisplayOne>(), 24);
        assert_eq!(core::mem::size_of::<VirtioGpuRespDisplayInfo>(), 24 + 16 * 24);
    }

    #[test]
    fn virtio_gpu_command_and_response_constants() {
        assert_eq!(VIRTIO_GPU_CMD_GET_DISPLAY_INFO, 0x0100);
        assert_eq!(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, 0x0101);
        assert_eq!(VIRTIO_GPU_CMD_SET_SCANOUT, 0x0103);
        assert_eq!(VIRTIO_GPU_CMD_RESOURCE_FLUSH, 0x0104);
        assert_eq!(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D, 0x0105);
        assert_eq!(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, 0x0106);
        assert_eq!(VIRTIO_GPU_RESP_OK_NODATA, 0x1100);
        assert_eq!(VIRTIO_GPU_RESP_OK_DISPLAY_INFO, 0x1101);
    }
}

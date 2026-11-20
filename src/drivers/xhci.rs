//! PCIe USB 3.0 xHCI (Extensible Host Controller Interface) Driver.
//!
//! Implements xHCI capability discovery, controller reset and configuration,
//! DCBAA, Command Ring, Event Ring with ERST, NOOP command submission & doorbell,
//! Command Completion Event polling, and root-hub port enumeration.

use core::ptr;
use crate::pmm::PhysicalPageAllocator;
use crate::drivers::pci::PciDeviceInfo;

pub const XHCI_MMIO_VA_BASE: u64 = 0xFFFF_FFFF_E300_0000;

// TRB Types (xHCI 1.1 Section 6.4.3 & 6.4.2)
pub const TRB_TYPE_NORMAL: u8 = 1;
pub const TRB_TYPE_SETUP_STAGE: u8 = 2;
pub const TRB_TYPE_DATA_STAGE: u8 = 3;
pub const TRB_TYPE_STATUS_STAGE: u8 = 4;
pub const TRB_TYPE_LINK: u8 = 6;
pub const TRB_TYPE_ENABLE_SLOT_CMD: u8 = 9;
pub const TRB_TYPE_DISABLE_SLOT_CMD: u8 = 10;
pub const TRB_TYPE_ADDRESS_DEVICE_CMD: u8 = 11;
pub const TRB_TYPE_CONFIGURE_ENDPOINT_CMD: u8 = 12;
pub const TRB_TYPE_EVALUATE_CONTEXT_CMD: u8 = 13;
pub const TRB_TYPE_NOOP_CMD: u8 = 23;
pub const TRB_TYPE_TRANSFER_EVENT: u8 = 32;
pub const TRB_TYPE_CMD_COMPLETION_EVENT: u8 = 33;
pub const TRB_TYPE_PORT_STATUS_CHANGE_EVENT: u8 = 34;

// Completion Codes (xHCI 1.1 Section 6.4.5)
pub const COMP_INVALID: u8 = 0;
pub const COMP_SUCCESS: u8 = 1;
pub const COMP_DATA_BUFFER_ERROR: u8 = 2;
pub const COMP_BABBLE_DETECTED: u8 = 3;
pub const COMP_USB_TRANSACTION_ERROR: u8 = 4;
pub const COMP_TRB_ERROR: u8 = 5;
pub const COMP_STALL_ERROR: u8 = 6;
pub const COMP_RESOURCE_ERROR: u8 = 7;
pub const COMP_BANDWIDTH_ERROR: u8 = 8;
pub const COMP_NO_SLOTS_AVAILABLE: u8 = 9;
pub const COMP_SHORT_PACKET: u8 = 13;

/// Standard 8-byte USB Setup Packet.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbSetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl UsbSetupPacket {
    pub fn new(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    pub fn get_descriptor(desc_type: u8, desc_index: u8, len: u16) -> Self {
        Self {
            request_type: 0x80,
            request: 6,
            value: ((desc_type as u16) << 8) | (desc_index as u16),
            index: 0,
            length: len,
        }
    }

    pub fn set_configuration(config_val: u8) -> Self {
        Self {
            request_type: 0x00,
            request: 9,
            value: config_val as u16,
            index: 0,
            length: 0,
        }
    }

    pub fn set_protocol(interface: u16, protocol: u16) -> Self {
        Self {
            request_type: 0x21,
            request: 0x0B,
            value: protocol,
            index: interface,
            length: 0,
        }
    }

    pub fn set_idle(interface: u16, duration: u8) -> Self {
        Self {
            request_type: 0x21,
            request: 0x0A,
            value: (duration as u16) << 8,
            index: interface,
            length: 0,
        }
    }

    pub fn as_u64(&self) -> u64 {
        (self.request_type as u64)
            | ((self.request as u64) << 8)
            | ((self.value as u64) << 16)
            | ((self.index as u64) << 32)
            | ((self.length as u64) << 48)
    }
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

/// Generic 16-byte Transfer Request Block (TRB).
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub const fn empty() -> Self {
        Self {
            param: 0,
            status: 0,
            control: 0,
        }
    }

    pub fn noop(cycle: bool) -> Self {
        let c = if cycle { 1u32 } else { 0u32 };
        Self {
            param: 0,
            status: 0,
            control: ((TRB_TYPE_NOOP_CMD as u32) << 10) | c,
        }
    }

    pub fn link(target_pa: u64, toggle_cycle: bool, cycle: bool) -> Self {
        let tc = if toggle_cycle { 1u32 << 1 } else { 0u32 };
        let c = if cycle { 1u32 } else { 0u32 };
        Self {
            param: target_pa,
            status: 0,
            control: ((TRB_TYPE_LINK as u32) << 10) | tc | c,
        }
    }

    pub fn cycle(&self) -> u8 {
        (self.control & 1) as u8
    }

    pub fn trb_type(&self) -> u8 {
        ((self.control >> 10) & 0x3F) as u8
    }

    pub fn completion_code(&self) -> u8 {
        ((self.status >> 24) & 0xFF) as u8
    }

    pub fn slot_id(&self) -> u8 {
        ((self.control >> 24) & 0xFF) as u8
    }
}

/// Event Ring Segment Table (ERST) Entry (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErstEntry {
    pub seg_base: u64,
    pub seg_size: u32,
    pub rsvd: u32,
}

/// Status of an individual root-hub port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPortInfo {
    pub port_id: u8,
    pub connected: bool,
    pub enabled: bool,
    pub speed: u8,
    pub power: bool,
}

/// State and memory buffers for an allocated xHCI Device Slot.
#[derive(Clone, Copy, Debug)]
pub struct XhciSlot {
    pub slot_id: u8,
    pub port_id: u8,
    pub speed: u8,
    pub out_ctx_pa: u64,
    pub out_ctx_va: u64,
    pub input_ctx_pa: u64,
    pub input_ctx_va: u64,
    pub ep0_ring_pa: u64,
    pub ep0_ring_va: u64,
    pub ep0_enqueue_idx: usize,
    pub ep0_pcs: u8,

    // Interrupt IN endpoint (e.g. DCI 3)
    pub intr_dci: u8,
    pub intr_ring_pa: u64,
    pub intr_ring_va: u64,
    pub intr_enqueue_idx: usize,
    pub intr_pcs: u8,
    pub report_buf_pa: u64,
    pub report_buf_va: u64,
}

impl XhciSlot {
    pub const fn empty() -> Self {
        Self {
            slot_id: 0,
            port_id: 0,
            speed: 0,
            out_ctx_pa: 0,
            out_ctx_va: 0,
            input_ctx_pa: 0,
            input_ctx_va: 0,
            ep0_ring_pa: 0,
            ep0_ring_va: 0,
            ep0_enqueue_idx: 0,
            ep0_pcs: 1,
            intr_dci: 0,
            intr_ring_pa: 0,
            intr_ring_va: 0,
            intr_enqueue_idx: 0,
            intr_pcs: 1,
            report_buf_pa: 0,
            report_buf_va: 0,
        }
    }

    pub unsafe fn enqueue_ep0_trb(&mut self, mut trb: Trb) {
        let cycle_bit = (self.ep0_pcs == 1) as u32;
        trb.control = (trb.control & !1) | cycle_bit;
        let ptr = (self.ep0_ring_va as *mut Trb).add(self.ep0_enqueue_idx);
        ptr::write_volatile(ptr, trb);
        clean_dcache_range(ptr as u64, 16);

        self.ep0_enqueue_idx += 1;
        if self.ep0_enqueue_idx >= 255 {
            let link = Trb::link(self.ep0_ring_pa, true, self.ep0_pcs == 1);
            let link_ptr = (self.ep0_ring_va as *mut Trb).add(255);
            ptr::write_volatile(link_ptr, link);
            clean_dcache_range(link_ptr as u64, 16);

            self.ep0_enqueue_idx = 0;
            self.ep0_pcs ^= 1;
        }
    }

    pub unsafe fn enqueue_intr_trb(&mut self, mut trb: Trb) {
        let cycle_bit = (self.intr_pcs == 1) as u32;
        trb.control = (trb.control & !1) | cycle_bit;
        let ptr = (self.intr_ring_va as *mut Trb).add(self.intr_enqueue_idx);
        ptr::write_volatile(ptr, trb);
        clean_dcache_range(ptr as u64, 16);

        self.intr_enqueue_idx += 1;
        if self.intr_enqueue_idx >= 255 {
            let link = Trb::link(self.intr_ring_pa, true, self.intr_pcs == 1);
            let link_ptr = (self.intr_ring_va as *mut Trb).add(255);
            ptr::write_volatile(link_ptr, link);
            clean_dcache_range(link_ptr as u64, 16);

            self.intr_enqueue_idx = 0;
            self.intr_pcs ^= 1;
        }
    }
}

/// xHCI Host Controller Device Instance.
#[derive(Clone, Copy, Debug)]
pub struct XhciDevice {
    pub base_va: u64,
    pub caplength: u8,
    pub hciversion: u16,
    pub max_slots: u8,
    pub max_intrs: u16,
    pub max_ports: u8,
    pub ctx_size: usize,
    pub dboff: u32,
    pub rtsoff: u32,
    pub op_base: u64,
    pub db_base: u64,
    pub rt_base: u64,

    // Memory buffer addresses
    pub dcbaa_pa: u64,
    pub dcbaa_va: u64,
    pub cmd_ring_pa: u64,
    pub cmd_ring_va: u64,
    pub evt_ring_pa: u64,
    pub evt_ring_va: u64,
    pub erst_pa: u64,
    pub erst_va: u64,

    // Ring state
    pub cmd_enqueue_idx: usize,
    pub cmd_pcs: u8, // Producer Cycle State
    pub evt_dequeue_idx: usize,
    pub evt_ccs: u8, // Consumer Cycle State

    // Slots
    pub slots: [Option<XhciSlot>; 16],

    // Verification outcomes
    pub noop_success: bool,
    pub noop_completion_code: u8,
}

impl XhciDevice {
    pub const fn empty() -> Self {
        Self {
            base_va: 0,
            caplength: 0,
            hciversion: 0,
            max_slots: 0,
            max_intrs: 0,
            max_ports: 0,
            ctx_size: 32,
            dboff: 0,
            rtsoff: 0,
            op_base: 0,
            db_base: 0,
            rt_base: 0,
            dcbaa_pa: 0,
            dcbaa_va: 0,
            cmd_ring_pa: 0,
            cmd_ring_va: 0,
            evt_ring_pa: 0,
            evt_ring_va: 0,
            erst_pa: 0,
            erst_va: 0,
            cmd_enqueue_idx: 0,
            cmd_pcs: 1,
            evt_dequeue_idx: 0,
            evt_ccs: 1,
            slots: [None; 16],
            noop_success: false,
            noop_completion_code: 0,
        }
    }

    /// Discover capability registers, halt, reset, configure rings, start controller,
    /// and verify NOOP command execution.
    pub unsafe fn init<const WORDS: usize>(
        &mut self,
        _pci_dev: &PciDeviceInfo,
        pmm: &mut PhysicalPageAllocator<WORDS>,
        hhdm_base: u64,
        mmio_va: u64,
    ) -> Result<(), &'static str> {
        self.base_va = mmio_va;

        // 1. Read Capability Registers (using 32-bit aligned MMIO reads)
        let cap_dw0 = ptr::read_volatile(mmio_va as *const u32);
        let caplength = (cap_dw0 & 0xFF) as u8;
        let hciversion = ((cap_dw0 >> 16) & 0xFFFF) as u16;
        let hcsparams1 = ptr::read_volatile((mmio_va + 0x04) as *const u32);
        let hcsparams2 = ptr::read_volatile((mmio_va + 0x08) as *const u32);
        let hccparams1 = ptr::read_volatile((mmio_va + 0x10) as *const u32);
        let dboff = ptr::read_volatile((mmio_va + 0x14) as *const u32) & !0x3;
        let rtsoff = ptr::read_volatile((mmio_va + 0x18) as *const u32) & !0x1F;

        self.caplength = caplength;
        self.hciversion = hciversion;
        self.max_slots = (hcsparams1 & 0xFF) as u8;
        self.max_intrs = ((hcsparams1 >> 8) & 0x7FF) as u16;
        self.max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;
        self.ctx_size = if ((hccparams1 >> 2) & 1) != 0 { 64 } else { 32 };
        self.dboff = dboff;
        self.rtsoff = rtsoff;
        self.op_base = mmio_va + (caplength as u64);
        self.db_base = mmio_va + (dboff as u64);
        self.rt_base = mmio_va + (rtsoff as u64);

        // 2. Halt Controller if running (USBCMD.RS = 0, wait USBSTS.HCH == 1)
        let usbsts = ptr::read_volatile((self.op_base + 0x04) as *const u32);
        if (usbsts & 1) == 0 {
            let usbcmd = ptr::read_volatile((self.op_base + 0x00) as *const u32);
            ptr::write_volatile((self.op_base + 0x00) as *mut u32, usbcmd & !1);
            let mut to = 100_000;
            while (ptr::read_volatile((self.op_base + 0x04) as *const u32) & 1) == 0 && to > 0 {
                to -= 1;
            }
            if to == 0 {
                return Err("XHCI_HALT_TIMEOUT");
            }
        }

        // 3. Reset Controller (USBCMD.HCRST = 1, wait HCRST == 0 and USBSTS.CNR == 0)
        ptr::write_volatile((self.op_base + 0x00) as *mut u32, 1 << 1);
        let mut to = 100_000;
        while (ptr::read_volatile((self.op_base + 0x00) as *const u32) & (1 << 1)) != 0 && to > 0 {
            to -= 1;
        }
        if to == 0 {
            return Err("XHCI_RESET_TIMEOUT");
        }
        let mut to = 100_000;
        while (ptr::read_volatile((self.op_base + 0x04) as *const u32) & (1 << 11)) != 0 && to > 0 {
            to -= 1;
        }
        if to == 0 {
            return Err("XHCI_CNR_TIMEOUT");
        }

        // 4. Configure Maximum Device Slots Enabled
        ptr::write_volatile((self.op_base + 0x38) as *mut u32, self.max_slots as u32);

        // 5. Allocate DCBAA (1 page = 4096 bytes)
        let dcbaa_pa = pmm.allocate_page().ok_or("XHCI_ALLOC_DCBAA_FAIL")?;
        let dcbaa_va = hhdm_base + dcbaa_pa;
        ptr::write_bytes(dcbaa_va as *mut u8, 0, 4096);

        // Check for scratchpad buffers
        let num_sp = (((hcsparams2 >> 21) & 0x1F) << 5) | ((hcsparams2 >> 27) & 0x1F);
        if num_sp > 0 {
            let sp_tbl_pa = pmm.allocate_page().ok_or("XHCI_ALLOC_SP_TBL_FAIL")?;
            let sp_tbl_va = hhdm_base + sp_tbl_pa;
            ptr::write_bytes(sp_tbl_va as *mut u8, 0, 4096);
            for i in 0..num_sp as usize {
                let sp_buf_pa = pmm.allocate_page().ok_or("XHCI_ALLOC_SP_BUF_FAIL")?;
                ptr::write_volatile((sp_tbl_va as *mut u64).add(i), sp_buf_pa);
            }
            clean_dcache_range(sp_tbl_va, 4096);
            ptr::write_volatile(dcbaa_va as *mut u64, sp_tbl_pa);
        }
        clean_dcache_range(dcbaa_va, 4096);
        ptr::write_volatile((self.op_base + 0x30) as *mut u64, dcbaa_pa);
        self.dcbaa_pa = dcbaa_pa;
        self.dcbaa_va = dcbaa_va;

        // 6. Allocate Command Ring (1 page = 256 TRBs)
        let cmd_pa = pmm.allocate_page().ok_or("XHCI_ALLOC_CMD_RING_FAIL")?;
        let cmd_va = hhdm_base + cmd_pa;
        ptr::write_bytes(cmd_va as *mut u8, 0, 4096);
        // Link TRB at index 255 pointing back to cmd_pa with Toggle Cycle = 1
        let link_trb = Trb::link(cmd_pa, true, true);
        ptr::write_volatile((cmd_va as *mut Trb).add(255), link_trb);
        clean_dcache_range(cmd_va, 4096);
        // Program CRCR with RCS = 1
        ptr::write_volatile((self.op_base + 0x18) as *mut u64, cmd_pa | 1);
        self.cmd_ring_pa = cmd_pa;
        self.cmd_ring_va = cmd_va;
        self.cmd_enqueue_idx = 0;
        self.cmd_pcs = 1;

        // 7. Allocate Event Ring + ERST Table
        let evt_pa = pmm.allocate_page().ok_or("XHCI_ALLOC_EVT_RING_FAIL")?;
        let evt_va = hhdm_base + evt_pa;
        ptr::write_bytes(evt_va as *mut u8, 0, 4096);
        clean_dcache_range(evt_va, 4096);

        let erst_pa = pmm.allocate_page().ok_or("XHCI_ALLOC_ERST_FAIL")?;
        let erst_va = hhdm_base + erst_pa;
        ptr::write_bytes(erst_va as *mut u8, 0, 4096);
        let erst_entry = ErstEntry {
            seg_base: evt_pa,
            seg_size: 256,
            rsvd: 0,
        };
        ptr::write_volatile(erst_va as *mut ErstEntry, erst_entry);
        clean_dcache_range(erst_va, 64);

        // Program Interrupter 0
        let intr0 = self.rt_base + 0x20;
        ptr::write_volatile((intr0 + 0x08) as *mut u32, 1); // ERSTSZ = 1
        ptr::write_volatile((intr0 + 0x10) as *mut u64, erst_pa); // ERSTBA
        ptr::write_volatile((intr0 + 0x18) as *mut u64, evt_pa | (1 << 3)); // ERDP with EHB=1
        ptr::write_volatile((intr0 + 0x00) as *mut u32, (1 << 1) | 1); // IMAN (IE=1, IP=1)
        self.evt_ring_pa = evt_pa;
        self.evt_ring_va = evt_va;
        self.erst_pa = erst_pa;
        self.erst_va = erst_va;
        self.evt_dequeue_idx = 0;
        self.evt_ccs = 1;

        // 8. Power on root hub ports
        for p in 1..=self.max_ports {
            let port_addr = (self.op_base + 0x400 + ((p - 1) as u64 * 0x10)) as *mut u32;
            let portsc = ptr::read_volatile(port_addr);
            let mask_rw1c = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23);
            let new_val = (portsc & !mask_rw1c) | (1 << 9); // PP = 1
            ptr::write_volatile(port_addr, new_val);
        }

        // 9. Start Controller (USBCMD.RS = 1, INTE = 1)
        let cmd = ptr::read_volatile((self.op_base + 0x00) as *const u32);
        ptr::write_volatile((self.op_base + 0x00) as *mut u32, cmd | 1 | (1 << 2));
        let mut to = 100_000;
        while (ptr::read_volatile((self.op_base + 0x04) as *const u32) & 1) != 0 && to > 0 {
            to -= 1;
        }
        if to == 0 {
            return Err("XHCI_START_TIMEOUT");
        }

        // 10. Submit NOOP Command and verify completion
        self.send_noop_command()
    }

    /// Submit a TRB to the Command Ring, managing the link TRB and cycle bit.
    pub unsafe fn enqueue_cmd_trb(&mut self, mut trb: Trb) {
        let cycle_bit = (self.cmd_pcs == 1) as u32;
        trb.control = (trb.control & !1) | cycle_bit;
        let trb_ptr = (self.cmd_ring_va as *mut Trb).add(self.cmd_enqueue_idx);
        ptr::write_volatile(trb_ptr, trb);
        clean_dcache_range(trb_ptr as u64, 16);

        self.cmd_enqueue_idx += 1;
        if self.cmd_enqueue_idx >= 255 {
            let link = Trb::link(self.cmd_ring_pa, true, self.cmd_pcs == 1);
            let link_ptr = (self.cmd_ring_va as *mut Trb).add(255);
            ptr::write_volatile(link_ptr, link);
            clean_dcache_range(link_ptr as u64, 16);

            self.cmd_enqueue_idx = 0;
            self.cmd_pcs ^= 1;
        }
    }

    /// Poll the Event Ring for the next event, advancing ERDP and toggling CCS if wrapping.
    pub unsafe fn poll_event(&mut self, max_spins: usize) -> Option<Trb> {
        let mut spins = max_spins;
        while spins > 0 {
            let evt_ptr = (self.evt_ring_va as *const Trb).add(self.evt_dequeue_idx);
            invalidate_dcache_range(evt_ptr as u64, 16);
            let trb = ptr::read_volatile(evt_ptr);

            if trb.cycle() == self.evt_ccs {
                self.evt_dequeue_idx += 1;
                if self.evt_dequeue_idx >= 256 {
                    self.evt_dequeue_idx = 0;
                    self.evt_ccs ^= 1;
                }

                let new_erdp = self.evt_ring_pa + (self.evt_dequeue_idx as u64 * 16);
                ptr::write_volatile((self.rt_base + 0x20 + 0x18) as *mut u64, new_erdp | (1 << 3));
                return Some(trb);
            }
            spins -= 1;
        }
        None
    }

    /// Submit a NOOP Command TRB on the Command Ring and wait for Command Completion Event.
    pub unsafe fn send_noop_command(&mut self) -> Result<(), &'static str> {
        let noop = Trb::noop(self.cmd_pcs == 1);
        self.enqueue_cmd_trb(noop);

        // Ring Doorbell 0 (Target 0 = Command Ring)
        ptr::write_volatile(self.db_base as *mut u32, 0);

        let mut attempts = 1_000_000;
        while let Some(trb) = self.poll_event(attempts) {
            let trb_type = trb.trb_type();
            let code = trb.completion_code();
            if trb_type == TRB_TYPE_CMD_COMPLETION_EVENT {
                self.noop_completion_code = code;
                if code == COMP_SUCCESS {
                    self.noop_success = true;
                    return Ok(());
                } else {
                    return Err("XHCI_NOOP_COMPLETION_ERROR");
                }
            }
            attempts -= 1;
            if attempts == 0 {
                break;
            }
        }
        Err("XHCI_NOOP_TIMEOUT")
    }

    /// Reset a root-hub port (1..=max_ports) via PORTSC.PR and clear change bit PRC.
    /// Returns the negotiated port speed.
    pub unsafe fn reset_port(&mut self, port: u8) -> Result<u8, &'static str> {
        if port == 0 || port > self.max_ports {
            return Err("INVALID_PORT");
        }
        let port_addr = (self.op_base + 0x400 + ((port - 1) as u64 * 0x10)) as *mut u32;
        let portsc = ptr::read_volatile(port_addr);
        if (portsc & 1) == 0 {
            return Err("PORT_NOT_CONNECTED");
        }

        // Preserve RW1C bits (bits 17..23) and write PR = 1 (bit 4)
        let mask_rw1c = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23);
        let new_val = (portsc & !mask_rw1c) | (1 << 4);
        ptr::write_volatile(port_addr, new_val);

        let mut to = 100_000;
        while to > 0 {
            let val = ptr::read_volatile(port_addr);
            if (val & (1 << 4)) == 0 {
                break;
            }
            to -= 1;
        }
        if to == 0 {
            return Err("PORT_RESET_TIMEOUT");
        }

        // Clear PRC (bit 21) by writing 1 to bit 21
        let val = ptr::read_volatile(port_addr);
        ptr::write_volatile(port_addr, (val & !mask_rw1c) | (1 << 21));

        let final_portsc = ptr::read_volatile(port_addr);
        let speed = ((final_portsc >> 10) & 0xF) as u8;
        Ok(speed)
    }

    /// Issue an Enable Slot Command TRB and return the assigned SlotID.
    pub unsafe fn enable_slot(&mut self) -> Result<u8, &'static str> {
        let trb = Trb {
            param: 0,
            status: 0,
            control: (TRB_TYPE_ENABLE_SLOT_CMD as u32) << 10,
        };
        self.enqueue_cmd_trb(trb);
        ptr::write_volatile(self.db_base as *mut u32, 0);

        let mut spins = 1_000_000;
        while let Some(evt) = self.poll_event(spins) {
            if evt.trb_type() == TRB_TYPE_CMD_COMPLETION_EVENT {
                if evt.completion_code() == COMP_SUCCESS {
                    return Ok(evt.slot_id());
                } else {
                    return Err("XHCI_ENABLE_SLOT_FAILED");
                }
            }
            spins -= 1;
            if spins == 0 {
                break;
            }
        }
        Err("XHCI_ENABLE_SLOT_TIMEOUT")
    }

    /// Issue an Address Device Command TRB (BSR = 0) with the provided input context.
    pub unsafe fn address_device(&mut self, slot_id: u8, input_ctx_pa: u64) -> Result<(), &'static str> {
        let trb = Trb {
            param: input_ctx_pa,
            status: 0,
            control: ((TRB_TYPE_ADDRESS_DEVICE_CMD as u32) << 10) | ((slot_id as u32) << 24),
        };
        self.enqueue_cmd_trb(trb);
        ptr::write_volatile(self.db_base as *mut u32, 0);

        let mut spins = 1_000_000;
        while let Some(evt) = self.poll_event(spins) {
            if evt.trb_type() == TRB_TYPE_CMD_COMPLETION_EVENT {
                if evt.completion_code() == COMP_SUCCESS {
                    return Ok(());
                } else {
                    return Err("XHCI_ADDRESS_DEVICE_FAILED");
                }
            }
            spins -= 1;
            if spins == 0 {
                break;
            }
        }
        Err("XHCI_ADDRESS_DEVICE_TIMEOUT")
    }

    /// Issue a Configure Endpoint Command TRB with the provided input context.
    pub unsafe fn configure_endpoint(&mut self, slot_id: u8, input_ctx_pa: u64) -> Result<(), &'static str> {
        let trb = Trb {
            param: input_ctx_pa,
            status: 0,
            control: ((TRB_TYPE_CONFIGURE_ENDPOINT_CMD as u32) << 10) | ((slot_id as u32) << 24),
        };
        self.enqueue_cmd_trb(trb);
        ptr::write_volatile(self.db_base as *mut u32, 0);

        let mut spins = 1_000_000;
        while let Some(evt) = self.poll_event(spins) {
            if evt.trb_type() == TRB_TYPE_CMD_COMPLETION_EVENT {
                if evt.completion_code() == COMP_SUCCESS {
                    return Ok(());
                } else {
                    return Err("XHCI_CONFIGURE_ENDPOINT_FAILED");
                }
            }
            spins -= 1;
            if spins == 0 {
                break;
            }
        }
        Err("XHCI_CONFIGURE_ENDPOINT_TIMEOUT")
    }

    /// Perform a standard USB Control Transfer on Endpoint 0 (Setup, optional Data, Status).
    pub unsafe fn control_transfer(
        &mut self,
        slot_id: u8,
        setup: UsbSetupPacket,
        data_buf_pa: u64,
        data_buf_va: u64,
        data_len: usize,
        dir_in: bool,
    ) -> Result<(), &'static str> {
        let slot = self.slots[slot_id as usize].as_mut().ok_or("XHCI_SLOT_NOT_ALLOCATED")?;

        let trt = if data_len == 0 {
            0u32
        } else if dir_in {
            3u32
        } else {
            2u32
        };
        let setup_trb = Trb {
            param: setup.as_u64(),
            status: 8,
            control: ((TRB_TYPE_SETUP_STAGE as u32) << 10) | (1 << 6) /* IDT */ | (trt << 16),
        };
        slot.enqueue_ep0_trb(setup_trb);

        if data_len > 0 {
            if !dir_in {
                clean_dcache_range(data_buf_va, data_len);
            }
            let dir_bit = if dir_in { 1u32 << 16 } else { 0u32 };
            let data_trb = Trb {
                param: data_buf_pa,
                status: data_len as u32,
                control: ((TRB_TYPE_DATA_STAGE as u32) << 10) | dir_bit,
            };
            slot.enqueue_ep0_trb(data_trb);
        }

        let status_dir = if data_len > 0 && dir_in {
            0u32
        } else {
            1u32 << 16
        };
        let status_trb = Trb {
            param: 0,
            status: 0,
            control: ((TRB_TYPE_STATUS_STAGE as u32) << 10) | (1 << 5) /* IOC */ | status_dir,
        };
        slot.enqueue_ep0_trb(status_trb);

        // Ring EP0 Doorbell (Doorbell slot_id, Target 1 = EP0)
        let db_addr = (self.db_base + (slot_id as u64 * 4)) as *mut u32;
        ptr::write_volatile(db_addr, 1);

        let mut spins = 1_000_000;
        while let Some(evt) = self.poll_event(spins) {
            if evt.trb_type() == TRB_TYPE_TRANSFER_EVENT && evt.slot_id() == slot_id {
                let code = evt.completion_code();
                if code == COMP_SUCCESS || code == COMP_SHORT_PACKET {
                    if data_len > 0 && dir_in {
                        invalidate_dcache_range(data_buf_va, data_len);
                    }
                    return Ok(());
                } else {
                    return Err("XHCI_CONTROL_XFER_FAILED");
                }
            }
            spins -= 1;
            if spins == 0 {
                break;
            }
        }
        Err("XHCI_CONTROL_XFER_TIMEOUT")
    }

    /// Read Root Hub Port Status & Control (PORTSC) for port (1..=max_ports)
    pub unsafe fn read_port_status(&self, port: u8) -> Option<UsbPortInfo> {
        if port == 0 || port > self.max_ports {
            return None;
        }
        let reg_addr = (self.op_base + 0x400 + ((port - 1) as u64 * 0x10)) as *const u32;
        let portsc = ptr::read_volatile(reg_addr);
        Some(UsbPortInfo {
            port_id: port,
            connected: (portsc & 1) != 0,
            enabled: ((portsc >> 1) & 1) != 0,
            speed: ((portsc >> 10) & 0xF) as u8,
            power: ((portsc >> 9) & 1) != 0,
        })
    }
}

pub unsafe fn write_input_control_context(ctx_va: u64, drop_flags: u32, add_flags: u32) {
    let p = ctx_va as *mut u32;
    ptr::write_volatile(p.add(0), drop_flags);
    ptr::write_volatile(p.add(1), add_flags);
    for i in 2..8 {
        ptr::write_volatile(p.add(i), 0);
    }
}

pub unsafe fn write_slot_context(
    ctx_va: u64,
    ctx_size: usize,
    is_input: bool,
    route_str: u32,
    speed: u8,
    entries: u8,
    root_port: u8,
) {
    let offset = if is_input { ctx_size } else { 0 };
    let p = (ctx_va + offset as u64) as *mut u32;
    let dw0 = (route_str & 0xF_FFFF) | (((speed as u32) & 0xF) << 20) | (((entries as u32) & 0x1F) << 27);
    let dw1 = ((root_port as u32) & 0xFF) << 16;
    ptr::write_volatile(p.add(0), dw0);
    ptr::write_volatile(p.add(1), dw1);
    for i in 2..8 {
        ptr::write_volatile(p.add(i), 0);
    }
    if ctx_size == 64 {
        for i in 8..16 {
            ptr::write_volatile(p.add(i), 0);
        }
    }
}

pub unsafe fn write_endpoint_context(
    ctx_va: u64,
    ctx_size: usize,
    is_input: bool,
    dci: u8,
    ep_type: u8,
    max_packet_size: u16,
    _max_burst: u8,
    interval: u8,
    tr_dequeue_ptr: u64,
    dcs: bool,
    cerr: u8,
    avg_trb_len: u16,
) {
    let offset = if is_input {
        (dci as usize + 1) * ctx_size
    } else {
        dci as usize * ctx_size
    };
    let p = (ctx_va + offset as u64) as *mut u32;
    let dw0 = ((interval as u32) & 0xFF) << 16;
    let dw1 = (((cerr as u32) & 0x3) << 1)
        | (((ep_type as u32) & 0x7) << 3)
        | (((max_packet_size as u32) & 0xFFFF) << 16);
    let dcs_bit = if dcs { 1u64 } else { 0u64 };
    let tr_ptr = (tr_dequeue_ptr & !0xF) | dcs_bit;
    let dw4 = (avg_trb_len as u32) & 0xFFFF;

    ptr::write_volatile(p.add(0), dw0);
    ptr::write_volatile(p.add(1), dw1);
    ptr::write_volatile(p.add(2), (tr_ptr & 0xFFFF_FFFF) as u32);
    ptr::write_volatile(p.add(3), ((tr_ptr >> 32) & 0xFFFF_FFFF) as u32);
    ptr::write_volatile(p.add(4), dw4);
    for i in 5..8 {
        ptr::write_volatile(p.add(i), 0);
    }
    if ctx_size == 64 {
        for i in 8..16 {
            ptr::write_volatile(p.add(i), 0);
        }
    }
}

pub fn port_speed_str(speed: u8) -> &'static str {
    match speed {
        1 => "Full-speed (12 Mb/s)",
        2 => "Low-speed (1.5 Mb/s)",
        3 => "High-speed (480 Mb/s)",
        4 => "SuperSpeed (5 Gb/s)",
        5 => "SuperSpeedPlus (10 Gb/s)",
        _ => "Unknown speed",
    }
}

pub fn format_xhci_info<'a>(xhci: &XhciDevice, buf: &'a mut [u8]) -> &'a str {
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

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut p = 0;
    let pre = b"xHCI v";
    buf[p..p + pre.len()].copy_from_slice(pre); p += pre.len();

    let maj = (xhci.hciversion >> 8) as u8;
    p += write_num(maj as u64, &mut buf[p..]);
    buf[p] = b'.'; p += 1;
    let min = ((xhci.hciversion >> 4) & 0xF) as u8;
    p += write_num(min as u64, &mut buf[p..]);

    let mid1 = b" (0x";
    buf[p..p + mid1.len()].copy_from_slice(mid1); p += mid1.len();
    buf[p] = HEX[((xhci.hciversion >> 12) & 0xF) as usize]; p += 1;
    buf[p] = HEX[((xhci.hciversion >> 8) & 0xF) as usize]; p += 1;
    buf[p] = HEX[((xhci.hciversion >> 4) & 0xF) as usize]; p += 1;
    buf[p] = HEX[(xhci.hciversion & 0xF) as usize]; p += 1;
    buf[p] = b')'; p += 1;

    let mid2 = b" | Slots: ";
    buf[p..p + mid2.len()].copy_from_slice(mid2); p += mid2.len();
    p += write_num(xhci.max_slots as u64, &mut buf[p..]);

    let mid3 = b" | Ports: ";
    buf[p..p + mid3.len()].copy_from_slice(mid3); p += mid3.len();
    p += write_num(xhci.max_ports as u64, &mut buf[p..]);

    core::str::from_utf8(&buf[..p]).unwrap_or("[xHCI Info]")
}

pub fn format_port_status<'a>(info: &UsbPortInfo, buf: &'a mut [u8]) -> &'a str {
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
    let pre = b"  Port ";
    buf[p..p + pre.len()].copy_from_slice(pre); p += pre.len();
    p += write_num(info.port_id as u64, &mut buf[p..]);
    let mid1 = b": ";
    buf[p..p + mid1.len()].copy_from_slice(mid1); p += mid1.len();

    if info.connected {
        let conn = b"Connected [";
        buf[p..p + conn.len()].copy_from_slice(conn); p += conn.len();
        let s = port_speed_str(info.speed).as_bytes();
        let slen = s.len().min(buf.len() - p - 2);
        buf[p..p + slen].copy_from_slice(&s[..slen]); p += slen;
        buf[p] = b']'; p += 1;
    } else {
        let dis = b"Disconnected";
        buf[p..p + dis.len()].copy_from_slice(dis); p += dis.len();
    }

    core::str::from_utf8(&buf[..p]).unwrap_or("[Port Status]")
}

static mut GLOBAL_XHCI: Option<XhciDevice> = None;

#[allow(static_mut_refs)]
pub unsafe fn register_global_xhci(dev: XhciDevice) {
    GLOBAL_XHCI = Some(dev);
}

#[allow(static_mut_refs)]
pub unsafe fn get_global_xhci() -> Option<&'static mut XhciDevice> {
    GLOBAL_XHCI.as_mut()
}

#[allow(static_mut_refs)]
pub unsafe fn get_global_xhci_ref() -> Option<&'static XhciDevice> {
    GLOBAL_XHCI.as_ref()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn xhci_trb_size_and_alignment() {
        assert_eq!(core::mem::size_of::<Trb>(), 16);
        assert_eq!(core::mem::align_of::<Trb>(), 16);
    }

    #[test]
    fn xhci_erst_entry_size_and_alignment() {
        assert_eq!(core::mem::size_of::<ErstEntry>(), 16);
        assert!(core::mem::align_of::<ErstEntry>() >= 8);
    }

    #[test]
    fn xhci_capability_offset_math() {
        // Base MMIO
        let mmio_base = 0xFFFF_FFFF_E300_0000u64;
        let caplength = 0x20u8;
        let dboff = 0x2000u32;
        let rtsoff = 0x3000u32;

        let op_base = mmio_base + caplength as u64;
        let db_base = mmio_base + dboff as u64;
        let rt_base = mmio_base + rtsoff as u64;

        assert_eq!(op_base, 0xFFFF_FFFF_E300_0020);
        assert_eq!(db_base, 0xFFFF_FFFF_E300_2000);
        assert_eq!(rt_base, 0xFFFF_FFFF_E300_3000);
    }

    #[test]
    fn xhci_noop_and_completion_trb_encoding() {
        // NOOP Command TRB
        let noop = Trb::noop(true);
        assert_eq!(noop.param, 0);
        assert_eq!(noop.status, 0);
        assert_eq!(noop.trb_type(), TRB_TYPE_NOOP_CMD);
        assert_eq!(noop.cycle(), 1);

        let noop_c0 = Trb::noop(false);
        assert_eq!(noop_c0.cycle(), 0);

        // Command Completion Event TRB
        let comp_trb = Trb {
            param: 0x4800_1000,
            status: (COMP_SUCCESS as u32) << 24,
            control: ((TRB_TYPE_CMD_COMPLETION_EVENT as u32) << 10) | 1 | (2 << 24),
        };
        assert_eq!(comp_trb.param, 0x4800_1000);
        assert_eq!(comp_trb.trb_type(), TRB_TYPE_CMD_COMPLETION_EVENT);
        assert_eq!(comp_trb.completion_code(), COMP_SUCCESS);
        assert_eq!(comp_trb.slot_id(), 2);
        assert_eq!(comp_trb.cycle(), 1);

        // Link TRB
        let link = Trb::link(0x4800_2000, true, true);
        assert_eq!(link.param, 0x4800_2000);
        assert_eq!(link.trb_type(), TRB_TYPE_LINK);
        assert_eq!(link.cycle(), 1);
        assert_eq!((link.control >> 1) & 1, 1); // TC bit
    }

    #[test]
    fn xhci_port_speed_description_test() {
        assert_eq!(port_speed_str(1), "Full-speed (12 Mb/s)");
        assert_eq!(port_speed_str(2), "Low-speed (1.5 Mb/s)");
        assert_eq!(port_speed_str(3), "High-speed (480 Mb/s)");
        assert_eq!(port_speed_str(4), "SuperSpeed (5 Gb/s)");
        assert_eq!(port_speed_str(5), "SuperSpeedPlus (10 Gb/s)");
        assert_eq!(port_speed_str(99), "Unknown speed");
    }

    #[test]
    fn xhci_formatting_helpers_test() {
        let mut xhci = XhciDevice::empty();
        xhci.hciversion = 0x0110;
        xhci.max_slots = 64;
        xhci.max_ports = 8;
        xhci.noop_success = true;

        let mut buf = [0u8; 128];
        let s = format_xhci_info(&xhci, &mut buf);
        assert!(s.contains("xHCI v1.1"));
        assert!(s.contains("Slots: 64"));
        assert!(s.contains("Ports: 8"));

        let port_conn = UsbPortInfo {
            port_id: 1,
            connected: true,
            enabled: true,
            speed: 1,
            power: true,
        };
        let mut pbuf = [0u8; 128];
        let ps = format_port_status(&port_conn, &mut pbuf);
        assert!(ps.contains("Port 1: Connected [Full-speed (12 Mb/s)]"));

        let port_dis = UsbPortInfo {
            port_id: 2,
            connected: false,
            enabled: false,
            speed: 0,
            power: true,
        };
        let mut pbuf2 = [0u8; 128];
        let ps2 = format_port_status(&port_dis, &mut pbuf2);
        assert!(ps2.contains("Port 2: Disconnected"));
    }

    #[test]
    fn usb_setup_packet_encoding_test() {
        let pkt = UsbSetupPacket::get_descriptor(1, 0, 18);
        assert_eq!(pkt.request_type, 0x80);
        assert_eq!(pkt.request, 6);
        assert_eq!(pkt.value, 0x0100);
        assert_eq!(pkt.index, 0);
        assert_eq!(pkt.length, 18);

        let val_u64 = pkt.as_u64();
        let bytes = val_u64.to_le_bytes();
        assert_eq!(bytes[0], 0x80);
        assert_eq!(bytes[1], 6);
        assert_eq!(bytes[2], 0x00);
        assert_eq!(bytes[3], 0x01);
        assert_eq!(bytes[4], 0x00);
        assert_eq!(bytes[5], 0x00);
        assert_eq!(bytes[6], 18);
        assert_eq!(bytes[7], 0);

        let set_cfg = UsbSetupPacket::set_configuration(1);
        assert_eq!(set_cfg.request_type, 0x00);
        assert_eq!(set_cfg.request, 9);
        assert_eq!(set_cfg.value, 1);
        assert_eq!(set_cfg.length, 0);

        let set_proto = UsbSetupPacket::set_protocol(0, 0);
        assert_eq!(set_proto.request_type, 0x21);
        assert_eq!(set_proto.request, 0x0B);

        let set_idle = UsbSetupPacket::set_idle(0, 0);
        assert_eq!(set_idle.request_type, 0x21);
        assert_eq!(set_idle.request, 0x0A);
    }

    #[test]
    fn xhci_context_writers_test() {
        let mut mem = [0u32; 128];
        let base_va = mem.as_mut_ptr() as u64;

        unsafe {
            // Test 32-byte context layout
            write_input_control_context(base_va, 0, 0x3);
            assert_eq!(mem[0], 0);
            assert_eq!(mem[1], 0x3);

            // Slot context (input -> offset 32 bytes = 8 u32)
            write_slot_context(base_va, 32, true, 0, 3, 1, 5);
            let slot_dw0 = mem[8];
            let slot_dw1 = mem[9];
            assert_eq!((slot_dw0 >> 20) & 0xF, 3); // speed 3
            assert_eq!((slot_dw0 >> 27) & 0x1F, 1); // entries 1
            assert_eq!((slot_dw1 >> 16) & 0xFF, 5); // root port 5

            // EP0 context (input DCI 1 -> offset (1 + 1)*32 = 64 bytes = 16 u32)
            write_endpoint_context(base_va, 32, true, 1, 4, 64, 0, 0, 0x5000_1000, true, 3, 8);
            let ep0_dw1 = mem[17];
            assert_eq!((ep0_dw1 >> 1) & 0x3, 3); // cerr 3
            assert_eq!((ep0_dw1 >> 3) & 0x7, 4); // ep_type 4 (Control)
            assert_eq!((ep0_dw1 >> 16) & 0xFFFF, 64); // max packet size 64

            let ep0_tr = (mem[18] as u64) | ((mem[19] as u64) << 32);
            assert_eq!(ep0_tr & !0xF, 0x5000_1000);
            assert_eq!(ep0_tr & 1, 1); // DCS bit
        }
    }
}

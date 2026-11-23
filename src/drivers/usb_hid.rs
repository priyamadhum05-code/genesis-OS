//! USB HID Keyboard and Mouse Driver for Project Genesis (Stage 19).
//!
//! Implements USB device descriptor parsing, HID Boot Protocol configuration,
//! interrupt transfer queue management, keycode-to-ASCII conversion, and
//! mouse pointer delta tracking.

use core::ptr;
use crate::pmm::PhysicalPageAllocator;
use crate::drivers::virtio_input::PointerState;
use crate::drivers::usb_msc::{register_global_usb_msc, UsbMscDevice};
use crate::drivers::xhci::{
    clean_dcache_range, get_global_xhci, invalidate_dcache_range, port_speed_str,
    write_endpoint_context, write_input_control_context, write_slot_context,
    Trb, UsbSetupPacket, XhciDevice, XhciSlot, COMP_SHORT_PACKET, COMP_SUCCESS,
    TRB_TYPE_NORMAL, TRB_TYPE_TRANSFER_EVENT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbDeviceKind {
    Unknown,
    Keyboard,
    Mouse,
    MassStorage,
    Hub,
    Other(u8),
}

impl UsbDeviceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            UsbDeviceKind::Keyboard => "HID Keyboard",
            UsbDeviceKind::Mouse => "HID Mouse",
            UsbDeviceKind::MassStorage => "USB Mass Storage",
            UsbDeviceKind::Hub => "USB Hub",
            UsbDeviceKind::Other(_) => "USB HID Other",
            UsbDeviceKind::Unknown => "Unknown Device",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbDeviceInfo {
    pub slot_id: u8,
    pub port_id: u8,
    pub speed: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub kind: UsbDeviceKind,
    pub intr_dci: u8,
}

pub static mut ENUMERATED_USB_DEVICES: [Option<UsbDeviceInfo>; 8] = [None; 8];
pub static mut ENUMERATED_USB_COUNT: usize = 0;

static mut KBD_FIFO: [u8; 64] = [0; 64];
static mut KBD_HEAD: usize = 0;
static mut KBD_TAIL: usize = 0;
static mut PREV_KEYS: [u8; 6] = [0; 6];

static mut MOUSE_STATE: PointerState = PointerState {
    x: 400,
    y: 300,
    left_button: false,
    right_button: false,
    scroll_delta: 0,
};
static mut MOUSE_CHANGED: bool = false;

pub unsafe fn push_keyboard_fifo(ch: u8) {
    let next = (KBD_HEAD + 1) % 64;
    if next != KBD_TAIL {
        KBD_FIFO[KBD_HEAD] = ch;
        KBD_HEAD = next;
    }
}

pub unsafe fn pop_keyboard_fifo() -> Option<u8> {
    if KBD_HEAD != KBD_TAIL {
        let ch = KBD_FIFO[KBD_TAIL];
        KBD_TAIL = (KBD_TAIL + 1) % 64;
        Some(ch)
    } else {
        None
    }
}

/// Convert a USB HID keycode (Usage Page 7) to ASCII byte, taking Shift into account.
pub fn hid_keycode_to_ascii(keycode: u8, shift: bool) -> Option<u8> {
    match keycode {
        0x04..=0x1D => {
            let base = if shift { b'A' } else { b'a' };
            Some(base + (keycode - 0x04))
        }
        0x1E..=0x26 => {
            if shift {
                let syms = b"!@#$%^&*";
                Some(syms[(keycode - 0x1E) as usize])
            } else {
                Some(b'1' + (keycode - 0x1E))
            }
        }
        0x27 => Some(if shift { b')' } else { b'0' }),
        0x28 => Some(b'\n'), // Enter
        0x29 => Some(27),    // Escape
        0x2A => Some(0x08),  // Backspace
        0x2B => Some(b'\t'), // Tab
        0x2C => Some(b' '),  // Space
        0x2D => Some(if shift { b'_' } else { b'-' }),
        0x2E => Some(if shift { b'+' } else { b'=' }),
        0x2F => Some(if shift { b'{' } else { b'[' }),
        0x30 => Some(if shift { b'}' } else { b']' }),
        0x31 => Some(if shift { b'|' } else { b'\\' }),
        0x33 => Some(if shift { b':' } else { b';' }),
        0x34 => Some(if shift { b'"' } else { b'\'' }),
        0x35 => Some(if shift { b'~' } else { b'`' }),
        0x36 => Some(if shift { b'<' } else { b',' }),
        0x37 => Some(if shift { b'>' } else { b'.' }),
        0x38 => Some(if shift { b'?' } else { b'/' }),
        _ => None,
    }
}

/// Format an enumerated USB device info into a human-readable string.
pub fn format_usb_device<'a>(dev: &UsbDeviceInfo, buf: &'a mut [u8]) -> &'a str {
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

    let pre = b"  Slot ";
    buf[p..p + pre.len()].copy_from_slice(pre); p += pre.len();
    p += write_num(dev.slot_id as u64, &mut buf[p..]);

    let mid1 = b": Port ";
    buf[p..p + mid1.len()].copy_from_slice(mid1); p += mid1.len();
    p += write_num(dev.port_id as u64, &mut buf[p..]);

    buf[p] = b' '; p += 1;
    buf[p] = b'['; p += 1;
    let s_speed = port_speed_str(dev.speed).as_bytes();
    let slen = s_speed.len().min(buf.len() - p - 30);
    buf[p..p + slen].copy_from_slice(&s_speed[..slen]); p += slen;
    buf[p] = b']'; p += 1;

    let mid2 = b" VID:";
    buf[p..p + mid2.len()].copy_from_slice(mid2); p += mid2.len();
    buf[p] = HEX[((dev.vendor_id >> 12) & 0xF) as usize]; p += 1;
    buf[p] = HEX[((dev.vendor_id >> 8) & 0xF) as usize]; p += 1;
    buf[p] = HEX[((dev.vendor_id >> 4) & 0xF) as usize]; p += 1;
    buf[p] = HEX[(dev.vendor_id & 0xF) as usize]; p += 1;

    let mid3 = b" PID:";
    buf[p..p + mid3.len()].copy_from_slice(mid3); p += mid3.len();
    buf[p] = HEX[((dev.product_id >> 12) & 0xF) as usize]; p += 1;
    buf[p] = HEX[((dev.product_id >> 8) & 0xF) as usize]; p += 1;
    buf[p] = HEX[((dev.product_id >> 4) & 0xF) as usize]; p += 1;
    buf[p] = HEX[(dev.product_id & 0xF) as usize]; p += 1;

    let mid4 = b" (";
    buf[p..p + mid4.len()].copy_from_slice(mid4); p += mid4.len();
    let kind_str = dev.kind.as_str().as_bytes();
    let klen = kind_str.len().min(buf.len() - p - 2);
    buf[p..p + klen].copy_from_slice(&kind_str[..klen]); p += klen;
    buf[p] = b')'; p += 1;

    core::str::from_utf8(&buf[..p]).unwrap_or("[USB Device]")
}

/// Enumerate all connected root hub ports on the xHCI controller, address devices,
/// configure endpoints, and prime interrupt transfer rings for HID keyboard/mouse.
pub unsafe fn enumerate_usb_devices<const WORDS: usize>(
    xhci: &mut XhciDevice,
    pmm: &mut PhysicalPageAllocator<WORDS>,
    hhdm_base: u64,
) -> usize {
    ENUMERATED_USB_COUNT = 0;
    for i in 0..8 {
        ENUMERATED_USB_DEVICES[i] = None;
    }
    KBD_HEAD = 0;
    KBD_TAIL = 0;
    PREV_KEYS = [0; 6];

    let ctrl_pa = match pmm.allocate_page() {
        Some(pa) => pa,
        None => return 0,
    };
    let ctrl_va = hhdm_base + ctrl_pa;

    for port in 1..=xhci.max_ports {
        let status = match xhci.read_port_status(port) {
            Some(s) => s,
            None => continue,
        };
        if !status.connected {
            continue;
        }

        let speed = match xhci.reset_port(port) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let slot_id = match xhci.enable_slot() {
            Ok(id) => id,
            Err(_) => continue,
        };
        if slot_id == 0 || slot_id as usize >= xhci.slots.len() {
            continue;
        }

        // Allocate Output Device Context (4096 bytes)
        let out_ctx_pa = match pmm.allocate_page() {
            Some(pa) => pa,
            None => continue,
        };
        let out_ctx_va = hhdm_base + out_ctx_pa;
        ptr::write_bytes(out_ctx_va as *mut u8, 0, 4096);
        clean_dcache_range(out_ctx_va, 4096);

        // Allocate Input Context (4096 bytes)
        let input_ctx_pa = match pmm.allocate_page() {
            Some(pa) => pa,
            None => continue,
        };
        let input_ctx_va = hhdm_base + input_ctx_pa;
        ptr::write_bytes(input_ctx_va as *mut u8, 0, 4096);

        // Allocate EP0 Transfer Ring (4096 bytes)
        let ep0_ring_pa = match pmm.allocate_page() {
            Some(pa) => pa,
            None => continue,
        };
        let ep0_ring_va = hhdm_base + ep0_ring_pa;
        ptr::write_bytes(ep0_ring_va as *mut u8, 0, 4096);
        let link_trb = Trb::link(ep0_ring_pa, true, true);
        ptr::write_volatile((ep0_ring_va as *mut Trb).add(255), link_trb);
        clean_dcache_range(ep0_ring_va, 4096);

        // Program DCBAA entry for this slot
        ptr::write_volatile((xhci.dcbaa_va as *mut u64).add(slot_id as usize), out_ctx_pa);
        clean_dcache_range(xhci.dcbaa_va + (slot_id as u64 * 8), 8);

        // Setup Input Context for Address Device
        let max_packet_size: u16 = match speed {
            2 => 8,       // Low-speed: 8
            1 => 64,      // Full-speed: 64
            3 => 64,      // High-speed: 64
            4 | 5 => 512, // SuperSpeed: 512
            _ => 64,
        };

        write_input_control_context(input_ctx_va, 0, (1 << 0) | (1 << 1));
        write_slot_context(input_ctx_va, xhci.ctx_size, true, 0, speed, 1, port);
        write_endpoint_context(
            input_ctx_va,
            xhci.ctx_size,
            true,
            1,
            4, // Control bidirectional
            max_packet_size,
            0,
            0,
            ep0_ring_pa,
            true,
            3,
            8,
        );
        clean_dcache_range(input_ctx_va, 4096);

        let mut slot = XhciSlot::empty();
        slot.slot_id = slot_id;
        slot.port_id = port;
        slot.speed = speed;
        slot.out_ctx_pa = out_ctx_pa;
        slot.out_ctx_va = out_ctx_va;
        slot.input_ctx_pa = input_ctx_pa;
        slot.input_ctx_va = input_ctx_va;
        slot.ep0_ring_pa = ep0_ring_pa;
        slot.ep0_ring_va = ep0_ring_va;
        xhci.slots[slot_id as usize] = Some(slot);

        // Address Device Command (BSR = 0)
        if xhci.address_device(slot_id, input_ctx_pa).is_err() {
            continue;
        }

        // Read Device Descriptor (18 bytes)
        let get_dev_desc = UsbSetupPacket::get_descriptor(1, 0, 18);
        if xhci.control_transfer(slot_id, get_dev_desc, ctrl_pa, ctrl_va, 18, true).is_err() {
            continue;
        }

        let dev_buf = core::slice::from_raw_parts(ctrl_va as *const u8, 18);
        let vid = (dev_buf[8] as u16) | ((dev_buf[9] as u16) << 8);
        let pid = (dev_buf[10] as u16) | ((dev_buf[11] as u16) << 8);

        // Check for USB Hub device (bDeviceClass == 0x09)
        if dev_buf[4] == 0x09 {
            let desc_len: u16 = if speed >= 4 { 12 } else { 8 };
            let get_hub = if speed >= 4 {
                UsbSetupPacket::get_ss_hub_descriptor(desc_len)
            } else {
                UsbSetupPacket::get_hub_descriptor(desc_len)
            };
            if xhci.control_transfer(slot_id, get_hub, ctrl_pa, ctrl_va, desc_len as usize, true).is_ok() {
                let hub_slice = core::slice::from_raw_parts(ctrl_va as *const u8, desc_len as usize);
                let num_hub_ports = hub_slice[2];

                write_input_control_context(input_ctx_va, 0, (1 << 0) | (1 << 1));
                crate::drivers::xhci::write_slot_context_ext(
                    input_ctx_va, xhci.ctx_size, true, 0, speed, 1, port,
                    true, num_hub_ports, 0, 0, false, 0,
                );
                clean_dcache_range(input_ctx_va, 4096);
                let _ = xhci.configure_endpoint(slot_id, input_ctx_pa);
                let _ = xhci.control_transfer(slot_id, UsbSetupPacket::set_configuration(1), 0, 0, 0, false);
                if speed >= 4 {
                    let _ = xhci.control_transfer(slot_id, UsbSetupPacket::set_hub_depth(0), 0, 0, 0, false);
                }

                if ENUMERATED_USB_COUNT < 8 {
                    let hub_dev_info = UsbDeviceInfo {
                        slot_id,
                        port_id: port,
                        speed,
                        vendor_id: vid,
                        product_id: pid,
                        kind: UsbDeviceKind::Hub,
                        intr_dci: 0,
                    };
                    ENUMERATED_USB_DEVICES[ENUMERATED_USB_COUNT] = Some(hub_dev_info);
                    ENUMERATED_USB_COUNT += 1;
                }

                // Power on downstream ports
                for hp in 1..=num_hub_ports {
                    let pwr = UsbSetupPacket::set_hub_port_feature(hp, 0x08); // PORT_POWER
                    let _ = xhci.control_transfer(slot_id, pwr, 0, 0, 0, false);
                }

                for _ in 0..10_000 { core::hint::spin_loop(); }

                for hp in 1..=num_hub_ports {
                    let st_pkt = UsbSetupPacket::get_hub_port_status(hp);
                    if xhci.control_transfer(slot_id, st_pkt, ctrl_pa, ctrl_va, 4, true).is_ok() {
                        let st = core::slice::from_raw_parts(ctrl_va as *const u8, 4);
                        if (st[0] & 1) != 0 {
                            let rst = UsbSetupPacket::set_hub_port_feature(hp, 0x04); // PORT_RESET
                            let _ = xhci.control_transfer(slot_id, rst, 0, 0, 0, false);
                            for _ in 0..20_000 { core::hint::spin_loop(); }

                            let child_speed = if speed >= 4 {
                                4 // SuperSpeed hub downstream ports operate at SuperSpeed
                            } else if xhci.control_transfer(slot_id, st_pkt, ctrl_pa, ctrl_va, 4, true).is_ok() {
                                let st2 = core::slice::from_raw_parts(ctrl_va as *const u8, 4);
                                let speed_bits = (st2[1] >> 1) & 0x3;
                                if speed_bits == 1 { 2 } else if speed_bits == 2 { 3 } else { 1 }
                            } else {
                                1
                            };

                            let child_slot_id = match xhci.enable_slot() {
                                Ok(id) if id != 0 && (id as usize) < xhci.slots.len() => id,
                                _ => continue,
                            };

                            let child_out_pa = match pmm.allocate_page() { Some(pa) => pa, None => continue };
                            let child_out_va = hhdm_base + child_out_pa;
                            ptr::write_bytes(child_out_va as *mut u8, 0, 4096);
                            clean_dcache_range(child_out_va, 4096);

                            let child_in_pa = match pmm.allocate_page() { Some(pa) => pa, None => continue };
                            let child_in_va = hhdm_base + child_in_pa;
                            ptr::write_bytes(child_in_va as *mut u8, 0, 4096);

                            let child_ep0_pa = match pmm.allocate_page() { Some(pa) => pa, None => continue };
                            let child_ep0_va = hhdm_base + child_ep0_pa;
                            ptr::write_bytes(child_ep0_va as *mut u8, 0, 4096);
                            let link = Trb::link(child_ep0_pa, true, true);
                            ptr::write_volatile((child_ep0_va as *mut Trb).add(255), link);
                            clean_dcache_range(child_ep0_va, 4096);

                            ptr::write_volatile((xhci.dcbaa_va as *mut u64).add(child_slot_id as usize), child_out_pa);
                            clean_dcache_range(xhci.dcbaa_va + (child_slot_id as u64 * 8), 8);

                            let child_max_pkt: u16 = match child_speed { 2 => 8, 1 => 64, 3 => 64, 4 => 512, _ => 64 };
                            let child_route = crate::drivers::xhci::calculate_child_route_string(0, 0, hp);

                            write_input_control_context(child_in_va, 0, (1 << 0) | (1 << 1));
                            crate::drivers::xhci::write_slot_context_ext(
                                child_in_va, xhci.ctx_size, true, child_route, child_speed, 1, port,
                                false, 0, slot_id, hp, false, 0,
                            );
                            write_endpoint_context(
                                child_in_va, xhci.ctx_size, true, 1, 4, child_max_pkt, 0, 0, child_ep0_pa, true, 3, 8,
                            );
                            clean_dcache_range(child_in_va, 4096);

                            let mut cslot = XhciSlot::empty();
                            cslot.slot_id = child_slot_id;
                            cslot.port_id = port;
                            cslot.speed = child_speed;
                            cslot.out_ctx_pa = child_out_pa;
                            cslot.out_ctx_va = child_out_va;
                            cslot.input_ctx_pa = child_in_pa;
                            cslot.input_ctx_va = child_in_va;
                            cslot.ep0_ring_pa = child_ep0_pa;
                            cslot.ep0_ring_va = child_ep0_va;
                            xhci.slots[child_slot_id as usize] = Some(cslot);

                            if xhci.address_device(child_slot_id, child_in_pa).is_ok() {
                                if xhci.control_transfer(child_slot_id, UsbSetupPacket::get_descriptor(1, 0, 18), ctrl_pa, ctrl_va, 18, true).is_ok() {
                                    let cdev_buf = core::slice::from_raw_parts(ctrl_va as *const u8, 18);
                                    let cvid = (cdev_buf[8] as u16) | ((cdev_buf[9] as u16) << 8);
                                    let cpid = (cdev_buf[10] as u16) | ((cdev_buf[11] as u16) << 8);

                                    if xhci.control_transfer(child_slot_id, UsbSetupPacket::get_descriptor(2, 0, 9), ctrl_pa, ctrl_va, 9, true).is_ok() {
                                        let ccfg_hdr = core::slice::from_raw_parts(ctrl_va as *const u8, 9);
                                        let ctot_len = ((ccfg_hdr[2] as u16) | ((ccfg_hdr[3] as u16) << 8)) as usize;
                                        let cfetch_len = ctot_len.clamp(9, 512);
                                        if xhci.control_transfer(child_slot_id, UsbSetupPacket::get_descriptor(2, 0, cfetch_len as u16), ctrl_pa, ctrl_va, cfetch_len, true).is_ok() {
                                            let ccfg_buf = core::slice::from_raw_parts(ctrl_va as *const u8, cfetch_len);
                                            let mut coff = 0;
                                            let mut ciface = 0u8;
                                            let mut ckind = UsbDeviceKind::Unknown;
                                            let mut cep: Option<(u8, u8, u16, u8, UsbDeviceKind)> = None;
                                            while coff + 2 <= cfetch_len {
                                                let cdlen = ccfg_buf[coff] as usize;
                                                if cdlen < 2 || coff + cdlen > cfetch_len { break; }
                                                let cdtype = ccfg_buf[coff + 1];
                                                if cdtype == 4 && cdlen >= 9 {
                                                    ciface = ccfg_buf[coff + 2];
                                                    let ciclass = ccfg_buf[coff + 5];
                                                    let ciproto = ccfg_buf[coff + 7];
                                                    if ciclass == 3 {
                                                        if ciproto == 1 { ckind = UsbDeviceKind::Keyboard; }
                                                        else if ciproto == 2 { ckind = UsbDeviceKind::Mouse; }
                                                        else { ckind = UsbDeviceKind::Other(ciproto); }
                                                    }
                                                } else if cdtype == 5 && cdlen >= 7 {
                                                    let cep_addr = ccfg_buf[coff + 2];
                                                    let cep_attr = ccfg_buf[coff + 3];
                                                    let cpkt = (ccfg_buf[coff + 4] as u16) | ((ccfg_buf[coff + 5] as u16) << 8);
                                                    let cint = ccfg_buf[coff + 6];
                                                    if (cep_addr & 0x80) != 0 && (cep_attr & 0x3) == 3 {
                                                        cep = Some((ciface, cep_addr & 0xF, cpkt, cint, ckind));
                                                    }
                                                }
                                                coff += cdlen;
                                            }

                                            let _ = xhci.control_transfer(child_slot_id, UsbSetupPacket::set_configuration(1), 0, 0, 0, false);
                                            if let Some((ci_num, ep_num, max_pkt, interval, k)) = cep {
                                                let _ = xhci.control_transfer(child_slot_id, UsbSetupPacket::set_protocol(ci_num as u16, 0), 0, 0, 0, false);
                                                let _ = xhci.control_transfer(child_slot_id, UsbSetupPacket::set_idle(ci_num as u16, 0), 0, 0, 0, false);
                                                let cdci = (ep_num * 2) + 1;
                                                if let Some(cring_pa) = pmm.allocate_page() {
                                                    let cring_va = hhdm_base + cring_pa;
                                                    ptr::write_bytes(cring_va as *mut u8, 0, 4096);
                                                    let clink = Trb::link(cring_pa, true, true);
                                                    ptr::write_volatile((cring_va as *mut Trb).add(255), clink);
                                                    clean_dcache_range(cring_va, 4096);

                                                    if let Some(crep_pa) = pmm.allocate_page() {
                                                        let crep_va = hhdm_base + crep_pa;
                                                        ptr::write_bytes(crep_va as *mut u8, 0, 4096);
                                                        clean_dcache_range(crep_va, 4096);

                                                        write_input_control_context(child_in_va, 0, (1 << 0) | (1 << cdci));
                                                        crate::drivers::xhci::write_slot_context_ext(
                                                            child_in_va, xhci.ctx_size, true, child_route, child_speed, cdci, port,
                                                            false, 0, slot_id, hp, false, 0,
                                                        );
                                                        write_endpoint_context(
                                                            child_in_va, xhci.ctx_size, true, cdci, 7, max_pkt, 0, interval, cring_pa, true, 3, max_pkt,
                                                        );
                                                        clean_dcache_range(child_in_va, 4096);
                                                        if xhci.configure_endpoint(child_slot_id, child_in_pa).is_ok() {
                                                            if let Some(s) = xhci.slots[child_slot_id as usize].as_mut() {
                                                                s.intr_dci = cdci;
                                                                s.intr_ring_pa = cring_pa;
                                                                s.intr_ring_va = cring_va;
                                                                s.report_buf_pa = crep_pa;
                                                                s.report_buf_va = crep_va;
                                                                s.enqueue_intr_trb(Trb {
                                                                    param: crep_pa,
                                                                    status: max_pkt as u32,
                                                                    control: ((TRB_TYPE_NORMAL as u32) << 10) | (1 << 5) | (1 << 2),
                                                                });
                                                            }
                                                            let cdb_addr = (xhci.db_base + (child_slot_id as u64 * 4)) as *mut u32;
                                                            ptr::write_volatile(cdb_addr, cdci as u32);
                                                            if ENUMERATED_USB_COUNT < 8 {
                                                                ENUMERATED_USB_DEVICES[ENUMERATED_USB_COUNT] = Some(UsbDeviceInfo {
                                                                    slot_id: child_slot_id,
                                                                    port_id: hp,
                                                                    speed: child_speed,
                                                                    vendor_id: cvid,
                                                                    product_id: cpid,
                                                                    kind: k,
                                                                    intr_dci: cdci,
                                                                });
                                                                ENUMERATED_USB_COUNT += 1;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            continue;
        }

        // Read Configuration Descriptor header (first 9 bytes to get wTotalLength)
        let get_cfg_header = UsbSetupPacket::get_descriptor(2, 0, 9);
        if xhci.control_transfer(slot_id, get_cfg_header, ctrl_pa, ctrl_va, 9, true).is_err() {
            continue;
        }

        let cfg_hdr = core::slice::from_raw_parts(ctrl_va as *const u8, 9);
        let total_len = ((cfg_hdr[2] as u16) | ((cfg_hdr[3] as u16) << 8)) as usize;
        let fetch_len = total_len.clamp(9, 512);

        // Read full configuration descriptors
        let get_cfg_full = UsbSetupPacket::get_descriptor(2, 0, fetch_len as u16);
        if xhci.control_transfer(slot_id, get_cfg_full, ctrl_pa, ctrl_va, fetch_len, true).is_err() {
            continue;
        }

        let cfg_buf = core::slice::from_raw_parts(ctrl_va as *const u8, fetch_len);

        // Parse descriptors for HID / MSC interfaces & endpoints
        let mut offset = 0;
        let mut cur_iface_num = 0u8;
        let mut is_hid_iface = false;
        let mut is_msc_iface = false;
        let mut detected_kind = UsbDeviceKind::Unknown;
        let mut found_ep: Option<(u8, u8, u16, u8, UsbDeviceKind)> = None;
        let mut msc_in_ep: Option<(u8, u16)> = None;
        let mut msc_out_ep: Option<(u8, u16)> = None;

        while offset + 2 <= fetch_len {
            let desc_len = cfg_buf[offset] as usize;
            if desc_len < 2 || offset + desc_len > fetch_len {
                break;
            }
            let desc_type = cfg_buf[offset + 1];

            if desc_type == 4 && desc_len >= 9 {
                // Interface Descriptor
                cur_iface_num = cfg_buf[offset + 2];
                let if_class = cfg_buf[offset + 5];
                let if_proto = cfg_buf[offset + 7];
                if if_class == 3 {
                    is_hid_iface = true;
                    is_msc_iface = false;
                    if if_proto == 1 {
                        detected_kind = UsbDeviceKind::Keyboard;
                    } else if if_proto == 2 {
                        detected_kind = UsbDeviceKind::Mouse;
                    } else {
                        detected_kind = UsbDeviceKind::Other(if_proto);
                    }
                } else if if_class == 8 {
                    is_msc_iface = true;
                    is_hid_iface = false;
                    detected_kind = UsbDeviceKind::MassStorage;
                } else {
                    is_hid_iface = false;
                    is_msc_iface = false;
                }
            } else if desc_type == 5 && desc_len >= 7 {
                // Endpoint Descriptor
                let ep_addr = cfg_buf[offset + 2];
                let ep_attr = cfg_buf[offset + 3];
                let max_pkt = (cfg_buf[offset + 4] as u16) | ((cfg_buf[offset + 5] as u16) << 8);
                let interval = cfg_buf[offset + 6];

                if is_hid_iface {
                    // Check Interrupt IN: bit 7 = 1, transfer type = 3 (Interrupt)
                    if (ep_addr & 0x80) != 0 && (ep_attr & 0x3) == 3 {
                        let ep_num = ep_addr & 0xF;
                        found_ep = Some((cur_iface_num, ep_num, max_pkt, interval, detected_kind));
                    }
                } else if is_msc_iface {
                    // Check Bulk: transfer type = 2 (Bulk)
                    if (ep_attr & 0x3) == 2 {
                        let ep_num = ep_addr & 0xF;
                        if (ep_addr & 0x80) != 0 {
                            msc_in_ep = Some((ep_num, max_pkt));
                        } else {
                            msc_out_ep = Some((ep_num, max_pkt));
                        }
                    }
                }
            }
            offset += desc_len;
        }

        // Set Configuration 1
        let set_cfg = UsbSetupPacket::set_configuration(1);
        let _ = xhci.control_transfer(slot_id, set_cfg, 0, 0, 0, false);

        if let Some((iface_num, ep_num, max_pkt, interval, kind)) = found_ep {
            // Set Boot Protocol (0)
            let set_proto = UsbSetupPacket::set_protocol(iface_num as u16, 0);
            let _ = xhci.control_transfer(slot_id, set_proto, 0, 0, 0, false);

            // Set Idle (0 duration = report only on change)
            let set_idle = UsbSetupPacket::set_idle(iface_num as u16, 0);
            let _ = xhci.control_transfer(slot_id, set_idle, 0, 0, 0, false);

            // Configure Interrupt IN Endpoint
            let dci = (ep_num * 2) + 1;

            let intr_ring_pa = match pmm.allocate_page() {
                Some(pa) => pa,
                None => continue,
            };
            let intr_ring_va = hhdm_base + intr_ring_pa;
            ptr::write_bytes(intr_ring_va as *mut u8, 0, 4096);
            let link = Trb::link(intr_ring_pa, true, true);
            ptr::write_volatile((intr_ring_va as *mut Trb).add(255), link);
            clean_dcache_range(intr_ring_va, 4096);

            let report_pa = match pmm.allocate_page() {
                Some(pa) => pa,
                None => continue,
            };
            let report_va = hhdm_base + report_pa;
            ptr::write_bytes(report_va as *mut u8, 0, 4096);
            clean_dcache_range(report_va, 4096);

            // Prepare Input Context for Configure Endpoint
            ptr::write_bytes(input_ctx_va as *mut u8, 0, 4096);
            write_input_control_context(input_ctx_va, 0, (1 << 0) | (1 << dci));
            write_slot_context(input_ctx_va, xhci.ctx_size, true, 0, speed, dci.max(1), port);
            let ep_interval = if interval == 0 { 3 } else { interval };
            write_endpoint_context(
                input_ctx_va,
                xhci.ctx_size,
                true,
                dci,
                7, // Interrupt IN
                max_pkt.max(8),
                0,
                ep_interval,
                intr_ring_pa,
                true,
                3,
                8,
            );
            clean_dcache_range(input_ctx_va, 4096);

            if xhci.configure_endpoint(slot_id, input_ctx_pa).is_ok() {
                if let Some(s) = xhci.slots[slot_id as usize].as_mut() {
                    s.intr_dci = dci;
                    s.intr_ring_pa = intr_ring_pa;
                    s.intr_ring_va = intr_ring_va;
                    s.report_buf_pa = report_pa;
                    s.report_buf_va = report_va;

                    // Enqueue initial Normal TRB to receive reports
                    let norm = Trb {
                        param: report_pa,
                        status: 8,
                        control: ((TRB_TYPE_NORMAL as u32) << 10) | (1 << 5) /* IOC */ | (1 << 2) /* ISP */,
                    };
                    s.enqueue_intr_trb(norm);
                }

                // Ring Doorbell for endpoint DCI
                let db_addr = (xhci.db_base + (slot_id as u64 * 4)) as *mut u32;
                ptr::write_volatile(db_addr, dci as u32);

                if ENUMERATED_USB_COUNT < 8 {
                    let info = UsbDeviceInfo {
                        slot_id,
                        port_id: port,
                        speed,
                        vendor_id: vid,
                        product_id: pid,
                        kind,
                        intr_dci: dci,
                    };
                    ENUMERATED_USB_DEVICES[ENUMERATED_USB_COUNT] = Some(info);
                    ENUMERATED_USB_COUNT += 1;
                }
            }
        } else if let (Some((in_ep, in_max_pkt)), Some((out_ep, out_max_pkt))) = (msc_in_ep, msc_out_ep) {
            // Configure Bulk IN & Bulk OUT endpoints for USB Mass Storage
            let in_dci = (in_ep * 2) + 1;
            let out_dci = out_ep * 2;

            let in_ring_pa = match pmm.allocate_page() {
                Some(pa) => pa,
                None => continue,
            };
            let in_ring_va = hhdm_base + in_ring_pa;
            ptr::write_bytes(in_ring_va as *mut u8, 0, 4096);
            let in_link = Trb::link(in_ring_pa, true, true);
            ptr::write_volatile((in_ring_va as *mut Trb).add(255), in_link);
            clean_dcache_range(in_ring_va, 4096);

            let out_ring_pa = match pmm.allocate_page() {
                Some(pa) => pa,
                None => continue,
            };
            let out_ring_va = hhdm_base + out_ring_pa;
            ptr::write_bytes(out_ring_va as *mut u8, 0, 4096);
            let out_link = Trb::link(out_ring_pa, true, true);
            ptr::write_volatile((out_ring_va as *mut Trb).add(255), out_link);
            clean_dcache_range(out_ring_va, 4096);

            let cbw_pa = match pmm.allocate_page() {
                Some(pa) => pa,
                None => continue,
            };
            let cbw_va = hhdm_base + cbw_pa;
            ptr::write_bytes(cbw_va as *mut u8, 0, 4096);
            clean_dcache_range(cbw_va, 4096);

            // Prepare Input Context for Configure Endpoint (enable Slot, in_dci, out_dci)
            ptr::write_bytes(input_ctx_va as *mut u8, 0, 4096);
            let add_flags = (1 << 0) | (1 << in_dci) | (1 << out_dci);
            write_input_control_context(input_ctx_va, 0, add_flags);
            let max_dci = in_dci.max(out_dci);
            write_slot_context(input_ctx_va, xhci.ctx_size, true, 0, speed, max_dci, port);

            // Bulk OUT (ep_type = 2)
            write_endpoint_context(
                input_ctx_va,
                xhci.ctx_size,
                true,
                out_dci,
                2, // Bulk OUT
                out_max_pkt.max(64),
                0,
                0,
                out_ring_pa,
                true,
                3,
                0,
            );

            // Bulk IN (ep_type = 6)
            write_endpoint_context(
                input_ctx_va,
                xhci.ctx_size,
                true,
                in_dci,
                6, // Bulk IN
                in_max_pkt.max(64),
                0,
                0,
                in_ring_pa,
                true,
                3,
                0,
            );
            clean_dcache_range(input_ctx_va, 4096);

            if xhci.configure_endpoint(slot_id, input_ctx_pa).is_ok() {
                if let Some(s) = xhci.slots[slot_id as usize].as_mut() {
                    s.bulk_in_dci = in_dci;
                    s.bulk_in_ring_pa = in_ring_pa;
                    s.bulk_in_ring_va = in_ring_va;
                    s.bulk_out_dci = out_dci;
                    s.bulk_out_ring_pa = out_ring_pa;
                    s.bulk_out_ring_va = out_ring_va;
                }

                let mut msc_dev = UsbMscDevice::empty();
                msc_dev.slot_id = slot_id;
                msc_dev.port_id = port;
                msc_dev.speed = speed;
                msc_dev.vendor_id = vid;
                msc_dev.product_id = pid;
                msc_dev.cbw_pa = cbw_pa;
                msc_dev.cbw_va = cbw_va;

                let scratch_pa = cbw_pa + 128;
                let scratch_va = cbw_va + 128;
                let _ = msc_dev.inquiry(xhci, scratch_pa, scratch_va);
                let _ = msc_dev.test_unit_ready(xhci, scratch_pa, scratch_va);
                let _ = msc_dev.read_capacity(xhci, scratch_pa, scratch_va);

                register_global_usb_msc(msc_dev);

                if ENUMERATED_USB_COUNT < 8 {
                    let info = UsbDeviceInfo {
                        slot_id,
                        port_id: port,
                        speed,
                        vendor_id: vid,
                        product_id: pid,
                        kind: UsbDeviceKind::MassStorage,
                        intr_dci: in_dci,
                    };
                    ENUMERATED_USB_DEVICES[ENUMERATED_USB_COUNT] = Some(info);
                    ENUMERATED_USB_COUNT += 1;
                }
            }
        }
    }

    ENUMERATED_USB_COUNT
}

/// Poll for incoming USB HID events from all active endpoints.
#[allow(static_mut_refs)]
pub unsafe fn poll_usb_events() {
    let xhci = match get_global_xhci() {
        Some(x) => x,
        None => return,
    };

    while let Some(evt) = xhci.poll_event(1) {
        if evt.trb_type() == TRB_TYPE_TRANSFER_EVENT {
            let slot_id = evt.slot_id();
            let code = evt.completion_code();
            if code == COMP_SUCCESS || code == COMP_SHORT_PACKET {
                // Find matching device info
                let mut matched_dev: Option<UsbDeviceInfo> = None;
                for i in 0..ENUMERATED_USB_COUNT {
                    if let Some(ref d) = ENUMERATED_USB_DEVICES[i] {
                        if d.slot_id == slot_id {
                            matched_dev = Some(*d);
                            break;
                        }
                    }
                }

                if let Some(dev) = matched_dev {
                    if let Some(slot) = xhci.slots[slot_id as usize].as_mut() {
                        let report_va = slot.report_buf_va;
                        let report_pa = slot.report_buf_pa;
                        let dci = slot.intr_dci;

                        if report_va != 0 {
                            invalidate_dcache_range(report_va, 8);
                            let rep = core::slice::from_raw_parts(report_va as *const u8, 8);

                            match dev.kind {
                                UsbDeviceKind::Keyboard => {
                                    let modifiers = rep[0];
                                    let shift = (modifiers & 0x22) != 0;
                                    let cur_keys = [rep[2], rep[3], rep[4], rep[5], rep[6], rep[7]];

                                    for &k in &cur_keys {
                                        if k != 0 {
                                            let mut already_pressed = false;
                                            for &pk in &PREV_KEYS {
                                                if pk == k {
                                                    already_pressed = true;
                                                    break;
                                                }
                                            }
                                            if !already_pressed {
                                                if let Some(ch) = hid_keycode_to_ascii(k, shift) {
                                                    push_keyboard_fifo(ch);
                                                }
                                            }
                                        }
                                    }
                                    PREV_KEYS = cur_keys;
                                }
                                UsbDeviceKind::Mouse => {
                                    let btn = rep[0];
                                    let dx = rep[1] as i8;
                                    let dy = rep[2] as i8;

                                    MOUSE_STATE.left_button = (btn & 1) != 0;
                                    MOUSE_STATE.right_button = (btn & 2) != 0;
                                    MOUSE_STATE.x += dx as i32;
                                    MOUSE_STATE.y += dy as i32;
                                    if rep.len() >= 4 {
                                        let wheel = rep[3] as i8;
                                        if wheel != 0 {
                                            MOUSE_STATE.scroll_delta += wheel as i32;
                                        }
                                    }
                                    MOUSE_CHANGED = true;
                                }
                                _ => {}
                            }

                            // Re-enqueue Normal TRB to continue receiving reports
                            let norm = Trb {
                                param: report_pa,
                                status: 8,
                                control: ((TRB_TYPE_NORMAL as u32) << 10) | (1 << 5) | (1 << 2),
                            };
                            slot.enqueue_intr_trb(norm);

                            let db_addr = (xhci.db_base + (slot_id as u64 * 4)) as *mut u32;
                            ptr::write_volatile(db_addr, dci as u32);
                        }
                    }
                }
            }
        }
    }
}

/// Poll USB HID keyboard for a received ASCII character (non-blocking).
pub unsafe fn poll_usb_keyboard() -> Option<u8> {
    poll_usb_events();
    pop_keyboard_fifo()
}

/// Poll USB HID mouse pointer state, clamping coordinates to screen bounds (non-blocking).
pub unsafe fn poll_usb_mouse(screen_w: u32, screen_h: u32) -> (PointerState, bool) {
    poll_usb_events();
    if screen_w > 0 && screen_h > 0 {
        MOUSE_STATE.x = MOUSE_STATE.x.clamp(0, screen_w as i32 - 1);
        MOUSE_STATE.y = MOUSE_STATE.y.clamp(0, screen_h as i32 - 1);
    }
    let changed = MOUSE_CHANGED || MOUSE_STATE.scroll_delta != 0;
    let res = MOUSE_STATE;
    MOUSE_STATE.scroll_delta = 0;
    MOUSE_CHANGED = false;
    (res, changed)
}

#[allow(static_mut_refs)]
pub unsafe fn get_enumerated_devices() -> (&'static [Option<UsbDeviceInfo>; 8], usize) {
    (&ENUMERATED_USB_DEVICES, ENUMERATED_USB_COUNT)
}

pub unsafe fn get_enumerated_count() -> usize {
    ENUMERATED_USB_COUNT
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn usb_hid_keycode_to_ascii_test() {
        // Letters
        assert_eq!(hid_keycode_to_ascii(0x04, false), Some(b'a'));
        assert_eq!(hid_keycode_to_ascii(0x04, true), Some(b'A'));
        assert_eq!(hid_keycode_to_ascii(0x1D, false), Some(b'z'));
        assert_eq!(hid_keycode_to_ascii(0x1D, true), Some(b'Z'));

        // Numbers & Symbols
        assert_eq!(hid_keycode_to_ascii(0x1E, false), Some(b'1'));
        assert_eq!(hid_keycode_to_ascii(0x1E, true), Some(b'!'));
        assert_eq!(hid_keycode_to_ascii(0x27, false), Some(b'0'));
        assert_eq!(hid_keycode_to_ascii(0x27, true), Some(b')'));

        // Control keys
        assert_eq!(hid_keycode_to_ascii(0x28, false), Some(b'\n'));
        assert_eq!(hid_keycode_to_ascii(0x29, false), Some(27));
        assert_eq!(hid_keycode_to_ascii(0x2A, false), Some(0x08));
        assert_eq!(hid_keycode_to_ascii(0x2B, false), Some(b'\t'));
        assert_eq!(hid_keycode_to_ascii(0x2C, false), Some(b' '));

        // Punctuation
        assert_eq!(hid_keycode_to_ascii(0x2D, false), Some(b'-'));
        assert_eq!(hid_keycode_to_ascii(0x2D, true), Some(b'_'));
        assert_eq!(hid_keycode_to_ascii(0x2E, false), Some(b'='));
        assert_eq!(hid_keycode_to_ascii(0x2E, true), Some(b'+'));

        // Unknown
        assert_eq!(hid_keycode_to_ascii(0x00, false), None);
        assert_eq!(hid_keycode_to_ascii(0xFF, false), None);
    }

    #[test]
    fn usb_hid_keyboard_fifo_and_rollover_test() {
        unsafe {
            KBD_HEAD = 0;
            KBD_TAIL = 0;
            PREV_KEYS = [0; 6];

            push_keyboard_fifo(b'h');
            push_keyboard_fifo(b'e');
            push_keyboard_fifo(b'l');
            push_keyboard_fifo(b'l');
            push_keyboard_fifo(b'o');

            assert_eq!(pop_keyboard_fifo(), Some(b'h'));
            assert_eq!(pop_keyboard_fifo(), Some(b'e'));
            assert_eq!(pop_keyboard_fifo(), Some(b'l'));
            assert_eq!(pop_keyboard_fifo(), Some(b'l'));
            assert_eq!(pop_keyboard_fifo(), Some(b'o'));
            assert_eq!(pop_keyboard_fifo(), None);
        }
    }

    #[test]
    fn usb_device_info_formatting_test() {
        let dev = UsbDeviceInfo {
            slot_id: 1,
            port_id: 5,
            speed: 3,
            vendor_id: 0x0627,
            product_id: 0x0001,
            kind: UsbDeviceKind::Keyboard,
            intr_dci: 3,
        };

        let mut buf = [0u8; 128];
        let s = format_usb_device(&dev, &mut buf);
        assert!(s.contains("Slot 1: Port 5"));
        assert!(s.contains("[High-speed (480 Mb/s)]"));
        assert!(s.contains("VID:0627"));
        assert!(s.contains("PID:0001"));
        assert!(s.contains("(HID Keyboard)"));
    }
}

//! VirtIO Input MMIO Keyboard Driver for Project Genesis (Stage 9B).
//!
//! Provides device probing across VirtIO MMIO slots for DeviceID == 18 (Input),
//! virtqueue 0 event queue setup with pre-posted receive buffers,
//! Linux EV_KEY event decoding into ASCII, and non-blocking key polling.

use crate::drivers::virtio_blk::{
    VirtqAvail, VirtqDesc, VirtqUsed, VIRTIO_MAGIC, VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_STRIDE,
    VIRTIO_REG_DEVICE_ID, VIRTIO_REG_DEV_FEAT, VIRTIO_REG_DEV_FEAT_SEL, VIRTIO_REG_DRV_FEAT,
    VIRTIO_REG_DRV_FEAT_SEL, VIRTIO_REG_GUEST_PAGE_SIZE, VIRTIO_REG_QUEUE_ALIGN,
    VIRTIO_REG_QUEUE_DESC_HI, VIRTIO_REG_QUEUE_DESC_LO, VIRTIO_REG_QUEUE_DEV_HI,
    VIRTIO_REG_QUEUE_DEV_LO, VIRTIO_REG_QUEUE_DRV_HI, VIRTIO_REG_QUEUE_DRV_LO,
    VIRTIO_REG_QUEUE_NOTIFY, VIRTIO_REG_QUEUE_NUM, VIRTIO_REG_QUEUE_NUM_MAX,
    VIRTIO_REG_QUEUE_PFN, VIRTIO_REG_QUEUE_READY, VIRTIO_REG_QUEUE_SEL, VIRTIO_REG_STATUS,
    VIRTIO_REG_VERSION, VIRTQ_DESC_F_WRITE,
};

pub const VIRTIO_DEV_INPUT: u32 = 18;

// Linux input event types
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;

// Absolute and relative axes
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;

// Mouse/tablet buttons
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

// Linux EV_KEY keycodes
pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_SPACE: u16 = 57;

pub const INPUT_QUEUE_SIZE: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct VirtioInputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerState {
    pub x: i32,
    pub y: i32,
    pub left_button: bool,
    pub right_button: bool,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            left_button: false,
            right_button: false,
        }
    }
}

/// Scales an EV_ABS absolute coordinate (0..32767) to screen bounds (0..max_dim).
#[inline]
pub fn scale_abs_coordinate(raw: u32, max_dim: u32) -> i32 {
    let scaled = (((raw as u64) * (max_dim as u64)) / 32767) as i32;
    scaled.clamp(0, max_dim as i32)
}

/// Clamps a relative coordinate delta within screen bounds.
#[inline]
pub fn update_rel_coordinate(current: i32, delta: i32, max_dim: u32) -> i32 {
    (current + delta).clamp(0, max_dim as i32)
}

impl PointerState {
    /// Updates pointer position and button state from a Linux input event.
    /// Returns true if coordinates or buttons changed.
    pub fn update_event(&mut self, ev: &VirtioInputEvent, screen_w: u32, screen_h: u32) -> bool {
        let max_w = if screen_w > 0 { screen_w - 1 } else { 0 };
        let max_h = if screen_h > 0 { screen_h - 1 } else { 0 };

        if ev.type_ == EV_ABS {
            if ev.code == ABS_X {
                let new_x = scale_abs_coordinate(ev.value, max_w);
                if self.x != new_x {
                    self.x = new_x;
                    return true;
                }
            } else if ev.code == ABS_Y {
                let new_y = scale_abs_coordinate(ev.value, max_h);
                if self.y != new_y {
                    self.y = new_y;
                    return true;
                }
            }
        } else if ev.type_ == EV_REL {
            if ev.code == REL_X {
                let new_x = update_rel_coordinate(self.x, ev.value as i32, max_w);
                if self.x != new_x {
                    self.x = new_x;
                    return true;
                }
            } else if ev.code == REL_Y {
                let new_y = update_rel_coordinate(self.y, ev.value as i32, max_h);
                if self.y != new_y {
                    self.y = new_y;
                    return true;
                }
            }
        } else if ev.type_ == EV_KEY {
            if ev.code == BTN_LEFT {
                let pressed = ev.value != 0;
                if self.left_button != pressed {
                    self.left_button = pressed;
                    return true;
                }
            } else if ev.code == BTN_RIGHT {
                let pressed = ev.value != 0;
                if self.right_button != pressed {
                    self.right_button = pressed;
                    return true;
                }
            }
        }

        false
    }
}

pub struct VirtIOInputDevice {
    pub base_va: u64,
    pub version: u32,
    pub queue_size: u16,
    pub queue_pa: u64,
    pub queue_va: u64,
    pub events_pa: u64,
    pub events_va: u64,
    pub avail_idx: u16,
    pub last_used_idx: u16,
    pub shift_pressed: bool,
}

impl VirtIOInputDevice {
    #[inline]
    unsafe fn read32(&self, offset: usize) -> u32 {
        core::ptr::read_volatile((self.base_va + offset as u64) as *const u32)
    }

    #[inline]
    unsafe fn write32(&self, offset: usize, val: u32) {
        core::ptr::write_volatile((self.base_va + offset as u64) as *mut u32, val)
    }

    /// Scans VirtIO MMIO slots for DeviceID == 18.
    /// Returns the number of input device slots found and stores their base VAs in `slots_va`.
    pub unsafe fn find_all_slots(base_va: u64, slots_va: &mut [u64]) -> usize {
        let mut count = 0;
        for slot in 0..VIRTIO_MMIO_SLOTS {
            let slot_va = base_va + (slot as u64) * VIRTIO_MMIO_STRIDE;
            let magic = core::ptr::read_volatile(slot_va as *const u32);
            if magic != VIRTIO_MAGIC {
                continue;
            }
            let dev_id = core::ptr::read_volatile((slot_va + VIRTIO_REG_DEVICE_ID as u64) as *const u32);
            if dev_id == VIRTIO_DEV_INPUT {
                if count < slots_va.len() {
                    slots_va[count] = slot_va;
                    count += 1;
                }
            }
        }
        count
    }

    /// Initializes a VirtIO Input device at the specified MMIO `slot_va`.
    /// `queue_pa` / `queue_va` must point to 2 contiguous physical pages (8192 bytes).
    pub unsafe fn init(
        slot_va: u64,
        queue_pa: u64,
        queue_va: u64,
    ) -> Result<Self, &'static str> {
        let events_pa = queue_pa + 512;
        let events_va = queue_va + 512;

        let mut dev = Self {
            base_va: slot_va,
            version: 0,
            queue_size: INPUT_QUEUE_SIZE as u16,
            queue_pa,
            queue_va,
            events_pa,
            events_va,
            avail_idx: 0,
            last_used_idx: 0,
            shift_pressed: false,
        };

        let version = dev.read32(VIRTIO_REG_VERSION);
        dev.version = version;

        // Reset
        dev.write32(VIRTIO_REG_STATUS, 0);
        // ACKNOWLEDGE (1)
        dev.write32(VIRTIO_REG_STATUS, 1);
        // DRIVER (2)
        let status = dev.read32(VIRTIO_REG_STATUS);
        dev.write32(VIRTIO_REG_STATUS, status | 2);

        // Feature negotiation
        if version >= 2 {
            dev.write32(VIRTIO_REG_DEV_FEAT_SEL, 0);
            let _f0 = dev.read32(VIRTIO_REG_DEV_FEAT);
            dev.write32(VIRTIO_REG_DEV_FEAT_SEL, 1);
            let f1 = dev.read32(VIRTIO_REG_DEV_FEAT);

            dev.write32(VIRTIO_REG_DRV_FEAT_SEL, 0);
            dev.write32(VIRTIO_REG_DRV_FEAT, 0);
            dev.write32(VIRTIO_REG_DRV_FEAT_SEL, 1);
            dev.write32(VIRTIO_REG_DRV_FEAT, f1 & 1); // VIRTIO_F_VERSION_1

            // FEATURES_OK (8)
            let cur = dev.read32(VIRTIO_REG_STATUS);
            dev.write32(VIRTIO_REG_STATUS, cur | 8);
            if (dev.read32(VIRTIO_REG_STATUS) & 8) == 0 {
                return Err("VirtIO-Input features rejected by device");
            }
        }

        // Configure Queue 0 (eventq)
        dev.write32(VIRTIO_REG_QUEUE_SEL, 0);
        let max_q = dev.read32(VIRTIO_REG_QUEUE_NUM_MAX);
        if max_q == 0 {
            return Err("VirtIO-Input eventq not available");
        }
        dev.write32(VIRTIO_REG_QUEUE_NUM, INPUT_QUEUE_SIZE as u32);

        // Zero queue memory (2 contiguous pages = 8192 bytes for legacy used ring alignment)
        core::ptr::write_bytes(queue_va as *mut u8, 0, 8192);

        if version >= 2 {
            let desc_pa = queue_pa;
            let avail_pa = queue_pa + 256;
            let used_pa = queue_pa + 2048;

            dev.write32(VIRTIO_REG_QUEUE_DESC_LO, (desc_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DESC_HI, (desc_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_LO, (avail_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DRV_HI, (avail_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_LO, (used_pa & 0xFFFF_FFFF) as u32);
            dev.write32(VIRTIO_REG_QUEUE_DEV_HI, (used_pa >> 32) as u32);
            dev.write32(VIRTIO_REG_QUEUE_READY, 1);
        } else {
            dev.write32(VIRTIO_REG_GUEST_PAGE_SIZE, 4096);
            dev.write32(VIRTIO_REG_QUEUE_ALIGN, 4096);
            dev.write32(VIRTIO_REG_QUEUE_PFN, (queue_pa >> 12) as u32);
        }

        // Pre-populate descriptors and avail ring with receive buffers
        let descs = queue_va as *mut VirtqDesc;
        let avail = (queue_va + 256) as *mut VirtqAvail<INPUT_QUEUE_SIZE>;
        for i in 0..INPUT_QUEUE_SIZE {
            let desc = descs.add(i);
            (*desc).addr = events_pa + (i as u64) * 8;
            (*desc).len = 8;
            (*desc).flags = VIRTQ_DESC_F_WRITE;
            (*desc).next = 0;
            (*avail).ring[i] = i as u16;
        }

        dev.avail_idx = INPUT_QUEUE_SIZE as u16;
        (*avail).flags = 0;
        (*avail).idx = dev.avail_idx;

        // DRIVER_OK (4)
        let cur = dev.read32(VIRTIO_REG_STATUS);
        dev.write32(VIRTIO_REG_STATUS, cur | 4);

        // Clean cache to PoC on descriptors and avail ring before queue notification
        #[cfg(target_os = "uefi")]
        {
            core::arch::asm!("dc cvac, {0}", in(reg) queue_va, options(nostack));
            core::arch::asm!("dc cvac, {0}", in(reg) (queue_va + 256), options(nostack));
            core::arch::asm!("dsb sy", options(nostack));
        }

        // Notify device that receive buffers are available
        dev.write32(VIRTIO_REG_QUEUE_NOTIFY, 0);

        Ok(dev)
    }

    /// Probes VirtIO MMIO slots for DeviceID == 18.
    /// Returns `Ok(Some(dev))` if an input device is found and initialized,
    /// `Ok(None)` if no input device exists (e.g. headless mode), or `Err(...)` on error.
    pub unsafe fn probe(
        base_va: u64,
        queue_pa: u64,
        queue_va: u64,
    ) -> Result<Option<Self>, &'static str> {
        let mut slots = [0u64; 1];
        if Self::find_all_slots(base_va, &mut slots) == 0 {
            return Ok(None);
        }
        Self::init(slots[0], queue_pa, queue_va).map(Some)
    }

    /// Polls the next raw event from the device queue, if available.
    pub unsafe fn poll_event(&mut self) -> Option<VirtioInputEvent> {
        let used_offset = if self.version >= 2 { 2048 } else { 4096 };
        let used = (self.queue_va + used_offset) as *mut VirtqUsed<INPUT_QUEUE_SIZE>;

        #[cfg(target_os = "uefi")]
        {
            core::arch::asm!("dc ivac, {}", in(reg) (self.queue_va + used_offset), options(nostack));
            core::arch::asm!("dsb sy", options(nostack));
        }

        let cur_used_idx = (*used).idx;
        if self.last_used_idx != cur_used_idx {
            let elem = (*used).ring[(self.last_used_idx as usize) % INPUT_QUEUE_SIZE];
            let desc_id = elem.id as usize;

            let event_ptr = (self.events_va + (desc_id as u64) * 8) as *const VirtioInputEvent;
            #[cfg(target_os = "uefi")]
            {
                core::arch::asm!("dc ivac, {}", in(reg) event_ptr, options(nostack));
                core::arch::asm!("dsb sy", options(nostack));
            }
            let event = *event_ptr;

            // Recycle buffer back into available ring
            let avail = (self.queue_va + 256) as *mut VirtqAvail<INPUT_QUEUE_SIZE>;
            (*avail).ring[(self.avail_idx as usize) % INPUT_QUEUE_SIZE] = desc_id as u16;
            self.avail_idx = self.avail_idx.wrapping_add(1);
            (*avail).idx = self.avail_idx;

            #[cfg(target_os = "uefi")]
            {
                core::arch::asm!("dc cvac, {}", in(reg) (self.queue_va + 256), options(nostack));
                core::arch::asm!("dsb sy", options(nostack));
            }

            self.write32(VIRTIO_REG_QUEUE_NOTIFY, 0);

            self.last_used_idx = self.last_used_idx.wrapping_add(1);

            return Some(event);
        }

        None
    }

    /// Polls for a key press event and translates it to an ASCII character.
    /// Non-blocking: returns `None` if no new key is available.
    pub unsafe fn poll_key(&mut self) -> Option<u8> {
        while let Some(event) = self.poll_event() {
            if event.type_ == EV_KEY {
                if event.code == KEY_LEFTSHIFT || event.code == KEY_RIGHTSHIFT {
                    self.shift_pressed = event.value != 0;
                } else if event.code < 0x100 && (event.value == 1 || event.value == 2) {
                    // Key press (1) or repeat (2)
                    if let Some(c) = ev_key_to_ascii(event.code, self.shift_pressed) {
                        return Some(c);
                    }
                }
            }
        }

        None
    }

    /// Polls all pending input events and updates `state` for tablet/mouse.
    /// Returns true if coordinates or button states changed.
    pub unsafe fn poll_pointer(
        &mut self,
        screen_w: u32,
        screen_h: u32,
        state: &mut PointerState,
    ) -> bool {
        let mut changed = false;
        while let Some(event) = self.poll_event() {
            if state.update_event(&event, screen_w, screen_h) {
                changed = true;
            }
        }
        changed
    }
}

/// Translates a Linux EV_KEY keycode into an ASCII character.
pub fn ev_key_to_ascii(code: u16, shift: bool) -> Option<u8> {
    match code {
        KEY_A => Some(if shift { b'A' } else { b'a' }),
        KEY_B => Some(if shift { b'B' } else { b'b' }),
        KEY_C => Some(if shift { b'C' } else { b'c' }),
        KEY_D => Some(if shift { b'D' } else { b'd' }),
        KEY_E => Some(if shift { b'E' } else { b'e' }),
        KEY_F => Some(if shift { b'F' } else { b'f' }),
        KEY_G => Some(if shift { b'G' } else { b'g' }),
        KEY_H => Some(if shift { b'H' } else { b'h' }),
        KEY_I => Some(if shift { b'I' } else { b'i' }),
        KEY_J => Some(if shift { b'J' } else { b'j' }),
        KEY_K => Some(if shift { b'K' } else { b'k' }),
        KEY_L => Some(if shift { b'L' } else { b'l' }),
        KEY_M => Some(if shift { b'M' } else { b'm' }),
        KEY_N => Some(if shift { b'N' } else { b'n' }),
        KEY_O => Some(if shift { b'O' } else { b'o' }),
        KEY_P => Some(if shift { b'P' } else { b'p' }),
        KEY_Q => Some(if shift { b'Q' } else { b'q' }),
        KEY_R => Some(if shift { b'R' } else { b'r' }),
        KEY_S => Some(if shift { b'S' } else { b's' }),
        KEY_T => Some(if shift { b'T' } else { b't' }),
        KEY_U => Some(if shift { b'U' } else { b'u' }),
        KEY_V => Some(if shift { b'V' } else { b'v' }),
        KEY_W => Some(if shift { b'W' } else { b'w' }),
        KEY_X => Some(if shift { b'X' } else { b'x' }),
        KEY_Y => Some(if shift { b'Y' } else { b'y' }),
        KEY_Z => Some(if shift { b'Z' } else { b'z' }),

        KEY_1 => Some(if shift { b'!' } else { b'1' }),
        KEY_2 => Some(if shift { b'@' } else { b'2' }),
        KEY_3 => Some(if shift { b'#' } else { b'3' }),
        KEY_4 => Some(if shift { b'$' } else { b'4' }),
        KEY_5 => Some(if shift { b'%' } else { b'5' }),
        KEY_6 => Some(if shift { b'^' } else { b'6' }),
        KEY_7 => Some(if shift { b'&' } else { b'7' }),
        KEY_8 => Some(if shift { b'*' } else { b'8' }),
        KEY_9 => Some(if shift { b'(' } else { b'9' }),
        KEY_0 => Some(if shift { b')' } else { b'0' }),

        KEY_MINUS => Some(if shift { b'_' } else { b'-' }),
        KEY_EQUAL => Some(if shift { b'+' } else { b'=' }),
        KEY_LEFTBRACE => Some(if shift { b'{' } else { b'[' }),
        KEY_RIGHTBRACE => Some(if shift { b'}' } else { b']' }),
        KEY_BACKSLASH => Some(if shift { b'|' } else { b'\\' }),
        KEY_SEMICOLON => Some(if shift { b':' } else { b';' }),
        KEY_APOSTROPHE => Some(if shift { b'"' } else { b'\'' }),
        KEY_GRAVE => Some(if shift { b'~' } else { b'`' }),
        KEY_COMMA => Some(if shift { b'<' } else { b',' }),
        KEY_DOT => Some(if shift { b'>' } else { b'.' }),
        KEY_SLASH => Some(if shift { b'?' } else { b'/' }),

        KEY_SPACE => Some(b' '),
        KEY_ENTER => Some(b'\n'),
        KEY_BACKSPACE => Some(0x08),
        KEY_TAB => Some(b'\t'),

        _ => None,
    }
}

// Global hooks for polling VirtIO-Input devices
pub static mut GLOBAL_VIRTIO_INPUT: Option<*mut VirtIOInputDevice> = None;
pub const MAX_INPUT_DEVICES: usize = 4;
pub static mut GLOBAL_INPUT_DEVICES: [*mut VirtIOInputDevice; MAX_INPUT_DEVICES] = [core::ptr::null_mut(); MAX_INPUT_DEVICES];
pub static mut GLOBAL_INPUT_COUNT: usize = 0;
pub static mut GLOBAL_POINTER_STATE: PointerState = PointerState { x: 400, y: 300, left_button: false, right_button: false };

pub unsafe fn register_global_virtio_input(dev: *mut VirtIOInputDevice) {
    let global_ptr = core::ptr::addr_of_mut!(GLOBAL_VIRTIO_INPUT);
    if (*global_ptr).is_none() {
        *global_ptr = Some(dev);
    }
    let count_ptr = core::ptr::addr_of_mut!(GLOBAL_INPUT_COUNT);
    let dev_arr_ptr = core::ptr::addr_of_mut!(GLOBAL_INPUT_DEVICES) as *mut *mut VirtIOInputDevice;
    for i in 0..*count_ptr {
        if *dev_arr_ptr.add(i) == dev {
            return;
        }
    }
    if *count_ptr < MAX_INPUT_DEVICES {
        *dev_arr_ptr.add(*count_ptr) = dev;
        *count_ptr += 1;
    }
}

pub unsafe fn poll_global_virtio_input() -> Option<u8> {
    let count_ptr = core::ptr::addr_of!(GLOBAL_INPUT_COUNT);
    let dev_arr_ptr = core::ptr::addr_of!(GLOBAL_INPUT_DEVICES) as *const *mut VirtIOInputDevice;
    for i in 0..*count_ptr {
        let dev = *dev_arr_ptr.add(i);
        if !dev.is_null() {
            if let Some(key) = (*dev).poll_key() {
                return Some(key);
            }
        }
    }
    let global_ptr = core::ptr::addr_of!(GLOBAL_VIRTIO_INPUT);
    if let Some(ptr) = *global_ptr {
        if *count_ptr == 0 {
            return (*ptr).poll_key();
        }
    }
    None
}

pub unsafe fn poll_global_pointer(screen_w: u32, screen_h: u32) -> (PointerState, bool) {
    let mut any_changed = false;
    let count_ptr = core::ptr::addr_of!(GLOBAL_INPUT_COUNT);
    let dev_arr_ptr = core::ptr::addr_of!(GLOBAL_INPUT_DEVICES) as *const *mut VirtIOInputDevice;
    let state_ptr = core::ptr::addr_of_mut!(GLOBAL_POINTER_STATE);

    for i in 0..*count_ptr {
        let dev = *dev_arr_ptr.add(i);
        if !dev.is_null() {
            if (*dev).poll_pointer(screen_w, screen_h, &mut *state_ptr) {
                any_changed = true;
            }
        }
    }
    (*state_ptr, any_changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtio_input_event_size_and_alignment() {
        assert_eq!(core::mem::size_of::<VirtioInputEvent>(), 8);
        assert_eq!(core::mem::align_of::<VirtioInputEvent>(), 4);
    }

    #[test]
    fn pointer_state_abs_scaling_and_clamping_test() {
        let mut state = PointerState { x: 400, y: 300, left_button: false, right_button: false };

        // Test scaling 0 -> 0
        let ev_x0 = VirtioInputEvent { type_: EV_ABS, code: ABS_X, value: 0 };
        assert!(state.update_event(&ev_x0, 800, 600));
        assert_eq!(state.x, 0);

        // Test scaling 32767 -> 799 (width - 1)
        let ev_xmax = VirtioInputEvent { type_: EV_ABS, code: ABS_X, value: 32767 };
        assert!(state.update_event(&ev_xmax, 800, 600));
        assert_eq!(state.x, 799);

        // Test scaling midpoint: 16383 -> ~399
        let ev_xmid = VirtioInputEvent { type_: EV_ABS, code: ABS_X, value: 16383 };
        assert!(state.update_event(&ev_xmid, 800, 600));
        assert!((state.x - 399).abs() <= 1);

        // Test Y scaling: 32767 -> 599 (height - 1)
        let ev_ymax = VirtioInputEvent { type_: EV_ABS, code: ABS_Y, value: 32767 };
        assert!(state.update_event(&ev_ymax, 800, 600));
        assert_eq!(state.y, 599);

        // Buttons
        let ev_lpress = VirtioInputEvent { type_: EV_KEY, code: BTN_LEFT, value: 1 };
        assert!(state.update_event(&ev_lpress, 800, 600));
        assert_eq!(state.left_button, true);

        let ev_lrelease = VirtioInputEvent { type_: EV_KEY, code: BTN_LEFT, value: 0 };
        assert!(state.update_event(&ev_lrelease, 800, 600));
        assert_eq!(state.left_button, false);

        let ev_rpress = VirtioInputEvent { type_: EV_KEY, code: BTN_RIGHT, value: 1 };
        assert!(state.update_event(&ev_rpress, 800, 600));
        assert_eq!(state.right_button, true);
    }

    #[test]
    fn pointer_state_rel_delta_and_clamping_test() {
        let mut state = PointerState { x: 100, y: 100, left_button: false, right_button: false };

        let ev_rel_x = VirtioInputEvent { type_: EV_REL, code: REL_X, value: 25 as u32 };
        assert!(state.update_event(&ev_rel_x, 800, 600));
        assert_eq!(state.x, 125);

        // Clamping negative
        let ev_rel_neg = VirtioInputEvent { type_: EV_REL, code: REL_X, value: (-500i32) as u32 };
        assert!(state.update_event(&ev_rel_neg, 800, 600));
        assert_eq!(state.x, 0);
    }

    #[test]
    fn keycode_to_ascii_letters_and_digits() {
        assert_eq!(ev_key_to_ascii(KEY_A, false), Some(b'a'));
        assert_eq!(ev_key_to_ascii(KEY_A, true), Some(b'A'));
        assert_eq!(ev_key_to_ascii(KEY_Z, false), Some(b'z'));
        assert_eq!(ev_key_to_ascii(KEY_Z, true), Some(b'Z'));

        assert_eq!(ev_key_to_ascii(KEY_1, false), Some(b'1'));
        assert_eq!(ev_key_to_ascii(KEY_1, true), Some(b'!'));
        assert_eq!(ev_key_to_ascii(KEY_0, false), Some(b'0'));
        assert_eq!(ev_key_to_ascii(KEY_0, true), Some(b')'));
    }

    #[test]
    fn keycode_to_ascii_symbols_and_control() {
        assert_eq!(ev_key_to_ascii(KEY_SPACE, false), Some(b' '));
        assert_eq!(ev_key_to_ascii(KEY_ENTER, false), Some(b'\n'));
        assert_eq!(ev_key_to_ascii(KEY_BACKSPACE, false), Some(0x08));
        assert_eq!(ev_key_to_ascii(KEY_TAB, false), Some(b'\t'));

        assert_eq!(ev_key_to_ascii(KEY_SLASH, false), Some(b'/'));
        assert_eq!(ev_key_to_ascii(KEY_SLASH, true), Some(b'?'));
        assert_eq!(ev_key_to_ascii(KEY_MINUS, true), Some(b'_'));
    }
}

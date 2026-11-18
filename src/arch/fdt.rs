//! Device Tree (FDT) Device Discovery for GIC and Timer.

#[derive(Clone, Copy, Debug)]
pub struct FdtGicInfo {
    pub is_v3: bool,
    pub gicd_base: u64,
    pub gicd_size: u64,
    pub gicr_base: u64,
    pub gicr_size: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct FdtTimerInfo {
    pub phys_ppi: u32,
    pub virt_ppi: u32,
}

#[derive(Clone, Copy)]
struct Node {
    parent_ac: u32,
    parent_sc: u32,
    child_ac: u32,
    child_sc: u32,
    is_gic: bool,
    is_gic_v3: bool,
    is_timer: bool,
    disabled: bool,
    reg_ptr: *const u8,
    reg_len: usize,
    irq_ptr: *const u8,
    irq_len: usize,
}

impl Node {
    const fn empty() -> Self {
        Self {
            parent_ac: 2,
            parent_sc: 1,
            child_ac: 2,
            child_sc: 1,
            is_gic: false,
            is_gic_v3: false,
            is_timer: false,
            disabled: false,
            reg_ptr: core::ptr::null(),
            reg_len: 0,
            irq_ptr: core::ptr::null(),
            irq_len: 0,
        }
    }
}

pub unsafe fn parse_fdt_devices(fdt: *const u8, cap: usize) -> (Option<FdtGicInfo>, Option<FdtTimerInfo>) {
    if !valid_fdt(fdt, cap) {
        return (None, None);
    }

    let total = be32(fdt.add(4)) as usize;
    let so = be32(fdt.add(8)) as usize;
    let stro = be32(fdt.add(12)) as usize;
    let ss = be32(fdt.add(32)) as usize;
    let sz = be32(fdt.add(36)) as usize;
    if so.checked_add(sz).unwrap_or(usize::MAX) > total || stro.checked_add(ss).unwrap_or(usize::MAX) > total {
        return (None, None);
    }

    let strings = fdt.add(stro);
    let (mut p, end) = (fdt.add(so), fdt.add(so + sz));
    let mut nodes = [Node::empty(); 32];
    let mut depth = 0usize;

    let mut found_gic: Option<FdtGicInfo> = None;
    let mut found_timer: Option<FdtTimerInfo> = None;

    while p.add(4) <= end {
        let token = be32(p);
        p = p.add(4);
        match token {
            1 => {
                // FDT_BEGIN_NODE
                if depth >= nodes.len() {
                    return (found_gic, found_timer);
                }
                let parent = if depth == 0 {
                    Node::empty()
                } else {
                    nodes[depth - 1]
                };
                nodes[depth] = Node {
                    parent_ac: parent.child_ac,
                    parent_sc: parent.child_sc,
                    child_ac: parent.child_ac,
                    child_sc: parent.child_sc,
                    is_gic: false,
                    is_gic_v3: false,
                    is_timer: false,
                    disabled: false,
                    reg_ptr: core::ptr::null(),
                    reg_len: 0,
                    irq_ptr: core::ptr::null(),
                    irq_len: 0,
                };
                depth += 1;
                while p < end && *p != 0 {
                    p = p.add(1);
                }
                p = align4(p.add(1));
            }
            2 => {
                // FDT_END_NODE
                if depth == 0 {
                    return (found_gic, found_timer);
                }
                depth -= 1;
                let n = nodes[depth];
                if !n.disabled {
                    if n.is_gic && found_gic.is_none() && !n.reg_ptr.is_null() {
                        let ac = n.parent_ac as usize;
                        let sc = n.parent_sc as usize;
                        let stride = (ac + sc) * 4;
                        if n.is_gic_v3 && n.reg_len >= stride * 2 {
                            let gicd_base = read_cells(n.reg_ptr, ac);
                            let gicd_size = read_cells(n.reg_ptr.add(ac * 4), sc);
                            let gicr_base = read_cells(n.reg_ptr.add(stride), ac);
                            let gicr_size = read_cells(n.reg_ptr.add(stride + ac * 4), sc);
                            found_gic = Some(FdtGicInfo {
                                is_v3: true,
                                gicd_base,
                                gicd_size,
                                gicr_base,
                                gicr_size,
                            });
                        } else if n.reg_len >= stride * 2 {
                            let gicd_base = read_cells(n.reg_ptr, ac);
                            let gicd_size = read_cells(n.reg_ptr.add(ac * 4), sc);
                            let gicc_base = read_cells(n.reg_ptr.add(stride), ac);
                            let gicc_size = read_cells(n.reg_ptr.add(stride + ac * 4), sc);
                            found_gic = Some(FdtGicInfo {
                                is_v3: false,
                                gicd_base,
                                gicd_size,
                                gicr_base: gicc_base,
                                gicr_size: gicc_size,
                            });
                        }
                    } else if n.is_timer && found_timer.is_none() && !n.irq_ptr.is_null() && n.irq_len >= 36 {
                        // 4 interrupts x 3 cells x 4 bytes = 48 bytes (or at least 3 interrupts = 36 bytes)
                        // Tuple 1: Non-secure physical timer (cells 3..5)
                        let phys_ppi = 16 + be32(n.irq_ptr.add(4 * 4)); // PPI num + 16
                        // Tuple 2: Virtual timer (cells 6..8)
                        let virt_ppi = 16 + be32(n.irq_ptr.add(7 * 4));
                        found_timer = Some(FdtTimerInfo {
                            phys_ppi,
                            virt_ppi,
                        });
                    }
                }
            }
            3 => {
                // FDT_PROP
                if depth == 0 || p.add(8) > end {
                    return (found_gic, found_timer);
                }
                let len = be32(p) as usize;
                let nameoff = be32(p.add(4)) as usize;
                p = p.add(8);
                if p.add(len) > end || nameoff >= ss {
                    return (found_gic, found_timer);
                }
                let name = cstr(strings.add(nameoff), ss - nameoff);
                let val = p;
                let node = &mut nodes[depth - 1];
                match name {
                    b"#address-cells" if len >= 4 => node.child_ac = be32(val),
                    b"#size-cells" if len >= 4 => node.child_sc = be32(val),
                    b"compatible" => {
                        if has_string(val, len, b"arm,gic-v3") {
                            node.is_gic = true;
                            node.is_gic_v3 = true;
                        } else if has_string(val, len, b"arm,cortex-a15-gic") || has_string(val, len, b"arm,gic-400") {
                            node.is_gic = true;
                            node.is_gic_v3 = false;
                        } else if has_string(val, len, b"arm,armv8-timer") {
                            node.is_timer = true;
                        }
                    }
                    b"status" => {
                        let status = cstr(val, len);
                        node.disabled = status == b"disabled" || status == b"fail";
                    }
                    b"reg" => {
                        node.reg_ptr = val;
                        node.reg_len = len;
                    }
                    b"interrupts" => {
                        node.irq_ptr = val;
                        node.irq_len = len;
                    }
                    _ => {}
                }
                p = align4(p.add(len));
            }
            4 => {
                // FDT_NOP: 4-byte token with no payload.
            }
            9 => break,
            _ => return (found_gic, found_timer),
        }
    }

    (found_gic, found_timer)
}

unsafe fn valid_fdt(p: *const u8, cap: usize) -> bool {
    if p.is_null() {
        return false;
    }
    let magic = be32(p);
    let total = be32(p.add(4)) as usize;
    magic == 0xd00dfeed && total >= 40 && total <= cap && be32(p.add(20)) >= 17 && be32(p.add(24)) <= 17
}

unsafe fn be32(p: *const u8) -> u32 {
    u32::from_be(core::ptr::read_unaligned(p as *const u32))
}

unsafe fn read_cells(p: *const u8, n: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..n {
        v = (v << 32) | be32(p.add(4 * i)) as u64;
    }
    v
}

unsafe fn align4(p: *const u8) -> *const u8 {
    ((p as usize + 3) & !3) as *const u8
}

unsafe fn cstr<'a>(p: *const u8, n: usize) -> &'a [u8] {
    let mut l = 0;
    while l < n && *p.add(l) != 0 {
        l += 1;
    }
    core::slice::from_raw_parts(p, l)
}

unsafe fn has_string(p: *const u8, n: usize, needle: &[u8]) -> bool {
    let mut i = 0;
    while i < n {
        let s = cstr(p.add(i), n - i);
        if s == needle {
            return true;
        }
        i += s.len() + 1;
    }
    false
}

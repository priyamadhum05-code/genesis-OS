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

#[derive(Clone, Copy, Debug)]
pub struct FdtPciInfo {
    pub ecam_base: u64,
    pub ecam_size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FdtAicInfo {
    pub is_v2: bool,
    pub base: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FdtSimpleFbInfo {
    pub base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: [u8; 16],
    pub format_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FdtUartInfo {
    pub base: u64,
    pub size: u64,
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

pub unsafe fn parse_fdt_pci(fdt: *const u8, cap: usize) -> Option<FdtPciInfo> {
    if !valid_fdt(fdt, cap) {
        return None;
    }

    let total = be32(fdt.add(4)) as usize;
    let so = be32(fdt.add(8)) as usize;
    let stro = be32(fdt.add(12)) as usize;
    let ss = be32(fdt.add(32)) as usize;
    let sz = be32(fdt.add(36)) as usize;
    if so.checked_add(sz).unwrap_or(usize::MAX) > total || stro.checked_add(ss).unwrap_or(usize::MAX) > total {
        return None;
    }

    let strings = fdt.add(stro);
    let (mut p, end) = (fdt.add(so), fdt.add(so + sz));
    let mut nodes = [Node::empty(); 32];
    let mut depth = 0usize;

    while p.add(4) <= end {
        let token = be32(p);
        p = p.add(4);
        match token {
            1 => {
                if depth >= nodes.len() {
                    return None;
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
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                let n = nodes[depth];
                if !n.disabled && n.is_gic && !n.reg_ptr.is_null() {
                    let ac = n.parent_ac as usize;
                    let sc = n.parent_sc as usize;
                    let stride = (ac + sc) * 4;
                    if n.reg_len >= stride {
                        let ecam_base = read_cells(n.reg_ptr, ac);
                        let ecam_size = read_cells(n.reg_ptr.add(ac * 4), sc);
                        return Some(FdtPciInfo {
                            ecam_base,
                            ecam_size,
                        });
                    }
                }
            }
            3 => {
                if depth == 0 || p.add(8) > end {
                    return None;
                }
                let len = be32(p) as usize;
                let nameoff = be32(p.add(4)) as usize;
                p = p.add(8);
                if p.add(len) > end || nameoff >= ss {
                    return None;
                }
                let name = cstr(strings.add(nameoff), ss - nameoff);
                let val = p;
                let node = &mut nodes[depth - 1];
                match name {
                    b"#address-cells" if len >= 4 => node.child_ac = be32(val),
                    b"#size-cells" if len >= 4 => node.child_sc = be32(val),
                    b"compatible" => {
                        if has_string(val, len, b"pci-host-ecam-generic") {
                            node.is_gic = true;
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
                    _ => {}
                }
                p = align4(p.add(len));
            }
            4 => {}
            9 => break,
            _ => return None,
        }
    }

    None
}

#[derive(Clone, Copy)]
struct AicNode {
    parent_ac: u32,
    parent_sc: u32,
    child_ac: u32,
    child_sc: u32,
    is_aic: bool,
    is_v2: bool,
    disabled: bool,
    reg_ptr: *const u8,
    reg_len: usize,
}

impl AicNode {
    const fn empty() -> Self {
        Self {
            parent_ac: 2,
            parent_sc: 1,
            child_ac: 2,
            child_sc: 1,
            is_aic: false,
            is_v2: false,
            disabled: false,
            reg_ptr: core::ptr::null(),
            reg_len: 0,
        }
    }
}

pub unsafe fn parse_fdt_aic(fdt: *const u8, cap: usize) -> Option<FdtAicInfo> {
    if !valid_fdt(fdt, cap) {
        return None;
    }

    let total = be32(fdt.add(4)) as usize;
    let so = be32(fdt.add(8)) as usize;
    let stro = be32(fdt.add(12)) as usize;
    let ss = be32(fdt.add(32)) as usize;
    let sz = be32(fdt.add(36)) as usize;
    if so.checked_add(sz).unwrap_or(usize::MAX) > total || stro.checked_add(ss).unwrap_or(usize::MAX) > total {
        return None;
    }

    let strings = fdt.add(stro);
    let (mut p, end) = (fdt.add(so), fdt.add(so + sz));
    let mut nodes = [AicNode::empty(); 32];
    let mut depth = 0usize;

    while p.add(4) <= end {
        let token = be32(p);
        p = p.add(4);
        match token {
            1 => {
                if depth >= nodes.len() {
                    return None;
                }
                let parent = if depth == 0 {
                    AicNode::empty()
                } else {
                    nodes[depth - 1]
                };
                nodes[depth] = AicNode {
                    parent_ac: parent.child_ac,
                    parent_sc: parent.child_sc,
                    child_ac: parent.child_ac,
                    child_sc: parent.child_sc,
                    is_aic: false,
                    is_v2: false,
                    disabled: false,
                    reg_ptr: core::ptr::null(),
                    reg_len: 0,
                };
                depth += 1;
                while p < end && *p != 0 {
                    p = p.add(1);
                }
                p = align4(p.add(1));
            }
            2 => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                let n = nodes[depth];
                if !n.disabled && n.is_aic && !n.reg_ptr.is_null() {
                    let ac = n.parent_ac as usize;
                    let sc = n.parent_sc as usize;
                    let stride = (ac + sc) * 4;
                    if n.reg_len >= stride {
                        let base = read_cells(n.reg_ptr, ac);
                        let size = read_cells(n.reg_ptr.add(ac * 4), sc);
                        return Some(FdtAicInfo {
                            is_v2: n.is_v2,
                            base,
                            size,
                        });
                    }
                }
            }
            3 => {
                if depth == 0 || p.add(8) > end {
                    return None;
                }
                let len = be32(p) as usize;
                let nameoff = be32(p.add(4)) as usize;
                p = p.add(8);
                if p.add(len) > end || nameoff >= ss {
                    return None;
                }
                let name = cstr(strings.add(nameoff), ss - nameoff);
                let val = p;
                let node = &mut nodes[depth - 1];
                match name {
                    b"#address-cells" if len >= 4 => node.child_ac = be32(val),
                    b"#size-cells" if len >= 4 => node.child_sc = be32(val),
                    b"compatible" => {
                        if has_string(val, len, b"apple,aic2") {
                            node.is_aic = true;
                            node.is_v2 = true;
                        } else if has_string(val, len, b"apple,aic") {
                            node.is_aic = true;
                            node.is_v2 = false;
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
                    _ => {}
                }
                p = align4(p.add(len));
            }
            4 => {}
            9 => break,
            _ => return None,
        }
    }

    None
}

#[derive(Clone, Copy)]
struct SimpleFbNode {
    parent_ac: u32,
    parent_sc: u32,
    child_ac: u32,
    child_sc: u32,
    is_simplefb: bool,
    disabled: bool,
    reg_ptr: *const u8,
    reg_len: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: [u8; 16],
    format_len: usize,
}

impl SimpleFbNode {
    const fn empty() -> Self {
        Self {
            parent_ac: 2,
            parent_sc: 1,
            child_ac: 2,
            child_sc: 1,
            is_simplefb: false,
            disabled: false,
            reg_ptr: core::ptr::null(),
            reg_len: 0,
            width: 0,
            height: 0,
            stride: 0,
            format: [0; 16],
            format_len: 0,
        }
    }
}

pub unsafe fn parse_fdt_simplefb(fdt: *const u8, cap: usize) -> Option<FdtSimpleFbInfo> {
    if !valid_fdt(fdt, cap) {
        return None;
    }

    let total = be32(fdt.add(4)) as usize;
    let so = be32(fdt.add(8)) as usize;
    let stro = be32(fdt.add(12)) as usize;
    let ss = be32(fdt.add(32)) as usize;
    let sz = be32(fdt.add(36)) as usize;
    if so.checked_add(sz).unwrap_or(usize::MAX) > total || stro.checked_add(ss).unwrap_or(usize::MAX) > total {
        return None;
    }

    let strings = fdt.add(stro);
    let (mut p, end) = (fdt.add(so), fdt.add(so + sz));
    let mut nodes = [SimpleFbNode::empty(); 32];
    let mut depth = 0usize;

    while p.add(4) <= end {
        let token = be32(p);
        p = p.add(4);
        match token {
            1 => {
                if depth >= nodes.len() {
                    return None;
                }
                let parent = if depth == 0 {
                    SimpleFbNode::empty()
                } else {
                    nodes[depth - 1]
                };
                nodes[depth] = SimpleFbNode {
                    parent_ac: parent.child_ac,
                    parent_sc: parent.child_sc,
                    child_ac: parent.child_ac,
                    child_sc: parent.child_sc,
                    is_simplefb: false,
                    disabled: false,
                    reg_ptr: core::ptr::null(),
                    reg_len: 0,
                    width: 0,
                    height: 0,
                    stride: 0,
                    format: [0; 16],
                    format_len: 0,
                };
                depth += 1;
                while p < end && *p != 0 {
                    p = p.add(1);
                }
                p = align4(p.add(1));
            }
            2 => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                let n = nodes[depth];
                if !n.disabled && n.is_simplefb && !n.reg_ptr.is_null() {
                    let ac = n.parent_ac as usize;
                    let sc = n.parent_sc as usize;
                    let stride_bytes = (ac + sc) * 4;
                    if n.reg_len >= stride_bytes {
                        let base = read_cells(n.reg_ptr, ac);
                        let size = read_cells(n.reg_ptr.add(ac * 4), sc);
                        return Some(FdtSimpleFbInfo {
                            base,
                            size,
                            width: n.width,
                            height: n.height,
                            stride: n.stride,
                            format: n.format,
                            format_len: n.format_len,
                        });
                    }
                }
            }
            3 => {
                if depth == 0 || p.add(8) > end {
                    return None;
                }
                let len = be32(p) as usize;
                let nameoff = be32(p.add(4)) as usize;
                p = p.add(8);
                if p.add(len) > end || nameoff >= ss {
                    return None;
                }
                let name = cstr(strings.add(nameoff), ss - nameoff);
                let val = p;
                let node = &mut nodes[depth - 1];
                match name {
                    b"#address-cells" if len >= 4 => node.child_ac = be32(val),
                    b"#size-cells" if len >= 4 => node.child_sc = be32(val),
                    b"compatible" => {
                        if has_string(val, len, b"simple-framebuffer") {
                            node.is_simplefb = true;
                        }
                    }
                    b"width" if len >= 4 => node.width = be32(val),
                    b"height" if len >= 4 => node.height = be32(val),
                    b"stride" if len >= 4 => node.stride = be32(val),
                    b"format" => {
                        let s = cstr(val, len);
                        let clen = s.len().min(16);
                        node.format[..clen].copy_from_slice(&s[..clen]);
                        node.format_len = clen;
                    }
                    b"status" => {
                        let status = cstr(val, len);
                        node.disabled = status == b"disabled" || status == b"fail";
                    }
                    b"reg" => {
                        node.reg_ptr = val;
                        node.reg_len = len;
                    }
                    _ => {}
                }
                p = align4(p.add(len));
            }
            4 => {}
            9 => break,
            _ => return None,
        }
    }

    None
}

#[derive(Clone, Copy)]
struct UartNode {
    parent_ac: u32,
    parent_sc: u32,
    child_ac: u32,
    child_sc: u32,
    is_uart: bool,
    disabled: bool,
    reg_ptr: *const u8,
    reg_len: usize,
}

impl UartNode {
    const fn empty() -> Self {
        Self {
            parent_ac: 2,
            parent_sc: 1,
            child_ac: 2,
            child_sc: 1,
            is_uart: false,
            disabled: false,
            reg_ptr: core::ptr::null(),
            reg_len: 0,
        }
    }
}

pub unsafe fn parse_fdt_apple_uart(fdt: *const u8, cap: usize) -> Option<FdtUartInfo> {
    if !valid_fdt(fdt, cap) {
        return None;
    }

    let total = be32(fdt.add(4)) as usize;
    let so = be32(fdt.add(8)) as usize;
    let stro = be32(fdt.add(12)) as usize;
    let ss = be32(fdt.add(32)) as usize;
    let sz = be32(fdt.add(36)) as usize;
    if so.checked_add(sz).unwrap_or(usize::MAX) > total || stro.checked_add(ss).unwrap_or(usize::MAX) > total {
        return None;
    }

    let strings = fdt.add(stro);
    let (mut p, end) = (fdt.add(so), fdt.add(so + sz));
    let mut nodes = [UartNode::empty(); 32];
    let mut depth = 0usize;

    while p.add(4) <= end {
        let token = be32(p);
        p = p.add(4);
        match token {
            1 => {
                if depth >= nodes.len() {
                    return None;
                }
                let parent = if depth == 0 {
                    UartNode::empty()
                } else {
                    nodes[depth - 1]
                };
                nodes[depth] = UartNode {
                    parent_ac: parent.child_ac,
                    parent_sc: parent.child_sc,
                    child_ac: parent.child_ac,
                    child_sc: parent.child_sc,
                    is_uart: false,
                    disabled: false,
                    reg_ptr: core::ptr::null(),
                    reg_len: 0,
                };
                depth += 1;
                while p < end && *p != 0 {
                    p = p.add(1);
                }
                p = align4(p.add(1));
            }
            2 => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                let n = nodes[depth];
                if !n.disabled && n.is_uart && !n.reg_ptr.is_null() {
                    let ac = n.parent_ac as usize;
                    let sc = n.parent_sc as usize;
                    let stride = (ac + sc) * 4;
                    if n.reg_len >= stride {
                        let base = read_cells(n.reg_ptr, ac);
                        let size = read_cells(n.reg_ptr.add(ac * 4), sc);
                        return Some(FdtUartInfo { base, size });
                    }
                }
            }
            3 => {
                if depth == 0 || p.add(8) > end {
                    return None;
                }
                let len = be32(p) as usize;
                let nameoff = be32(p.add(4)) as usize;
                p = p.add(8);
                if p.add(len) > end || nameoff >= ss {
                    return None;
                }
                let name = cstr(strings.add(nameoff), ss - nameoff);
                let val = p;
                let node = &mut nodes[depth - 1];
                match name {
                    b"#address-cells" if len >= 4 => node.child_ac = be32(val),
                    b"#size-cells" if len >= 4 => node.child_sc = be32(val),
                    b"compatible" => {
                        if has_string(val, len, b"apple,s5l-uart") || has_string(val, len, b"apple,uart") {
                            node.is_uart = true;
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
                    _ => {}
                }
                p = align4(p.add(len));
            }
            4 => {}
            9 => break,
            _ => return None,
        }
    }

    None
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

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::vec::Vec;

    struct MockFdtBuilder {
        struct_block: Vec<u8>,
        string_block: Vec<u8>,
    }

    impl MockFdtBuilder {
        fn new() -> Self {
            Self {
                struct_block: Vec::new(),
                string_block: Vec::new(),
            }
        }

        fn add_string(&mut self, s: &[u8]) -> u32 {
            let offset = self.string_block.len() as u32;
            self.string_block.extend_from_slice(s);
            self.string_block.push(0);
            offset
        }

        fn begin_node(&mut self, name: &str) {
            self.struct_block.extend_from_slice(&1u32.to_be_bytes()); // FDT_BEGIN_NODE
            self.struct_block.extend_from_slice(name.as_bytes());
            self.struct_block.push(0);
            while self.struct_block.len() % 4 != 0 {
                self.struct_block.push(0);
            }
        }

        fn end_node(&mut self) {
            self.struct_block.extend_from_slice(&2u32.to_be_bytes()); // FDT_END_NODE
        }

        fn add_prop(&mut self, name: &[u8], val: &[u8]) {
            let nameoff = self.add_string(name);
            self.struct_block.extend_from_slice(&3u32.to_be_bytes()); // FDT_PROP
            self.struct_block.extend_from_slice(&(val.len() as u32).to_be_bytes());
            self.struct_block.extend_from_slice(&nameoff.to_be_bytes());
            self.struct_block.extend_from_slice(val);
            while self.struct_block.len() % 4 != 0 {
                self.struct_block.push(0);
            }
        }

        fn build(&mut self) -> Vec<u8> {
            self.struct_block.extend_from_slice(&9u32.to_be_bytes()); // FDT_END

            let header_size = 40;
            let so = header_size;
            let sz = self.struct_block.len();
            let stro = so + sz;
            let ss = self.string_block.len();
            let total = stro + ss;

            let mut fdt = Vec::new();
            fdt.extend_from_slice(&0xd00dfeed_u32.to_be_bytes()); // magic
            fdt.extend_from_slice(&(total as u32).to_be_bytes());
            fdt.extend_from_slice(&(so as u32).to_be_bytes());
            fdt.extend_from_slice(&(stro as u32).to_be_bytes());
            fdt.extend_from_slice(&40u32.to_be_bytes()); // off_mem_rsvmap
            fdt.extend_from_slice(&17u32.to_be_bytes()); // version
            fdt.extend_from_slice(&17u32.to_be_bytes()); // last_comp_version
            fdt.extend_from_slice(&0u32.to_be_bytes());  // boot_cpuid_phys
            fdt.extend_from_slice(&(ss as u32).to_be_bytes()); // size_dt_strings
            fdt.extend_from_slice(&(sz as u32).to_be_bytes()); // size_dt_struct

            fdt.extend_from_slice(&self.struct_block);
            fdt.extend_from_slice(&self.string_block);
            fdt
        }
    }

    #[test]
    fn parse_fdt_aic_test() {
        let mut b = MockFdtBuilder::new();
        b.begin_node("");
        b.add_prop(b"#address-cells", &2u32.to_be_bytes());
        b.add_prop(b"#size-cells", &2u32.to_be_bytes());

        b.begin_node("interrupt-controller@28e100000");
        b.add_prop(b"compatible", b"apple,aic2\0");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x0000_0002_8E10_0000_u64.to_be_bytes()); // base
        reg.extend_from_slice(&0x0000_0000_0004_0000_u64.to_be_bytes()); // size
        b.add_prop(b"reg", &reg);
        b.end_node();

        b.end_node();
        let fdt = b.build();

        unsafe {
            let aic_info = parse_fdt_aic(fdt.as_ptr(), fdt.len()).expect("failed to parse aic");
            assert!(aic_info.is_v2);
            assert_eq!(aic_info.base, 0x2_8E10_0000);
            assert_eq!(aic_info.size, 0x4_0000);
        }
    }

    #[test]
    fn parse_fdt_simplefb_test() {
        let mut b = MockFdtBuilder::new();
        b.begin_node("");
        b.add_prop(b"#address-cells", &2u32.to_be_bytes());
        b.add_prop(b"#size-cells", &2u32.to_be_bytes());

        b.begin_node("framebuffer@80000000");
        b.add_prop(b"compatible", b"simple-framebuffer\0");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x0000_0000_8000_0000_u64.to_be_bytes());
        reg.extend_from_slice(&0x0000_0000_0100_0000_u64.to_be_bytes());
        b.add_prop(b"reg", &reg);
        b.add_prop(b"width", &1920u32.to_be_bytes());
        b.add_prop(b"height", &1080u32.to_be_bytes());
        b.add_prop(b"stride", &(1920u32 * 4).to_be_bytes());
        b.add_prop(b"format", b"a8r8g8b8\0");
        b.end_node();

        b.end_node();
        let fdt = b.build();

        unsafe {
            let fb = parse_fdt_simplefb(fdt.as_ptr(), fdt.len()).expect("failed to parse simplefb");
            assert_eq!(fb.base, 0x8000_0000);
            assert_eq!(fb.size, 0x0100_0000);
            assert_eq!(fb.width, 1920);
            assert_eq!(fb.height, 1080);
            assert_eq!(fb.stride, 1920 * 4);
            assert_eq!(&fb.format[..fb.format_len], b"a8r8g8b8");
        }
    }

    #[test]
    fn parse_fdt_apple_uart_test() {
        let mut b = MockFdtBuilder::new();
        b.begin_node("");
        b.add_prop(b"#address-cells", &2u32.to_be_bytes());
        b.add_prop(b"#size-cells", &2u32.to_be_bytes());

        b.begin_node("serial@235200000");
        b.add_prop(b"compatible", b"apple,s5l-uart\0apple,uart\0");
        let mut reg = Vec::new();
        reg.extend_from_slice(&0x0000_0002_3520_0000_u64.to_be_bytes());
        reg.extend_from_slice(&0x0000_0000_0000_4000_u64.to_be_bytes());
        b.add_prop(b"reg", &reg);
        b.end_node();

        b.end_node();
        let fdt = b.build();

        unsafe {
            let uart = parse_fdt_apple_uart(fdt.as_ptr(), fdt.len()).expect("failed to parse apple_uart");
            assert_eq!(uart.base, 0x2_3520_0000);
            assert_eq!(uart.size, 0x4000);
        }
    }
}

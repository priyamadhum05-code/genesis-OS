//! USB Mass Storage (MSC) Bulk-Only Transport (BOT) & SCSI Driver (Stage 20).
//!
//! Implements USB Mass Storage BOT protocol with CBW/CSW handling,
//! SCSI INQUIRY, TEST UNIT READY, REQUEST SENSE, READ CAPACITY (10),
//! and READ (10) operations over xHCI Bulk endpoints.

use core::ptr;
use crate::drivers::xhci::{clean_dcache_range, invalidate_dcache_range, XhciDevice};

pub const CBW_SIGNATURE: u32 = 0x43425355; // "USBC"
pub const CSW_SIGNATURE: u32 = 0x53425355; // "USBS"

pub const SCSI_CMD_TEST_UNIT_READY: u8 = 0x00;
pub const SCSI_CMD_REQUEST_SENSE: u8 = 0x03;
pub const SCSI_CMD_INQUIRY: u8 = 0x12;
pub const SCSI_CMD_READ_CAPACITY_10: u8 = 0x25;
pub const SCSI_CMD_READ_10: u8 = 0x28;
pub const SCSI_CMD_WRITE_10: u8 = 0x2A;

/// USB BOT Command Block Wrapper (31 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct CommandBlockWrapper {
    pub signature: u32,
    pub tag: u32,
    pub data_transfer_len: u32,
    pub flags: u8,
    pub lun: u8,
    pub cdb_len: u8,
    pub cdb: [u8; 16],
}

impl CommandBlockWrapper {
    pub fn new(tag: u32, data_len: u32, is_in: bool, cdb: &[u8]) -> Self {
        let mut block = [0u8; 16];
        let len = cdb.len().min(16);
        block[..len].copy_from_slice(&cdb[..len]);

        Self {
            signature: CBW_SIGNATURE,
            tag,
            data_transfer_len: data_len,
            flags: if is_in { 0x80 } else { 0x00 },
            lun: 0,
            cdb_len: len as u8,
            cdb: block,
        }
    }
}

/// USB BOT Command Status Wrapper (13 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CommandStatusWrapper {
    pub signature: u32,
    pub tag: u32,
    pub data_residue: u32,
    pub status: u8,
}

/// USB Mass Storage Device Instance.
#[derive(Clone, Copy, Debug)]
pub struct UsbMscDevice {
    pub slot_id: u8,
    pub port_id: u8,
    pub speed: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub vendor_name: [u8; 8],
    pub product_name: [u8; 16],
    pub revision: [u8; 4],
    pub total_blocks: u64,
    pub block_size: u32,

    pub cbw_pa: u64,
    pub cbw_va: u64,
    pub tag: u32,
    pub is_initialized: bool,
}

impl UsbMscDevice {
    pub const fn empty() -> Self {
        Self {
            slot_id: 0,
            port_id: 0,
            speed: 0,
            vendor_id: 0,
            product_id: 0,
            vendor_name: [b' '; 8],
            product_name: [b' '; 16],
            revision: [b' '; 4],
            total_blocks: 0,
            block_size: 512,
            cbw_pa: 0,
            cbw_va: 0,
            tag: 1,
            is_initialized: false,
        }
    }

    /// Execute a SCSI command wrapped in a Bulk-Only Transport transaction (CBW -> Data -> CSW).
    pub unsafe fn exec_scsi_cmd(
        &mut self,
        xhci: &mut XhciDevice,
        cdb: &[u8],
        is_in: bool,
        data_pa: u64,
        data_va: u64,
        data_len: usize,
    ) -> Result<(), &'static str> {
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1);
        if self.tag == 0 {
            self.tag = 1;
        }

        let cbw = CommandBlockWrapper::new(tag, data_len as u32, is_in, cdb);
        ptr::write_volatile(self.cbw_va as *mut CommandBlockWrapper, cbw);
        clean_dcache_range(self.cbw_va, 31);

        // 1. Send CBW (31 bytes) via Bulk OUT
        xhci.bulk_transfer_out(self.slot_id, self.cbw_pa, self.cbw_va, 31)?;

        // 2. Data Stage (if requested)
        if data_len > 0 {
            if is_in {
                xhci.bulk_transfer_in(self.slot_id, data_pa, data_va, data_len)?;
            } else {
                xhci.bulk_transfer_out(self.slot_id, data_pa, data_va, data_len)?;
            }
        }

        // 3. Receive CSW (13 bytes) via Bulk IN
        let csw_pa = self.cbw_pa + 64;
        let csw_va = self.cbw_va + 64;
        ptr::write_bytes(csw_va as *mut u8, 0, 13);
        clean_dcache_range(csw_va, 13);

        xhci.bulk_transfer_in(self.slot_id, csw_pa, csw_va, 13)?;
        invalidate_dcache_range(csw_va, 13);

        let csw = ptr::read_volatile(csw_va as *const CommandStatusWrapper);
        if csw.signature != CSW_SIGNATURE {
            return Err("USB_MSC_CSW_SIG_MISMATCH");
        }
        if csw.tag != tag {
            return Err("USB_MSC_CSW_TAG_MISMATCH");
        }
        if csw.status != 0 {
            return Err("USB_MSC_SCSI_ERROR");
        }

        Ok(())
    }

    /// Issue SCSI INQUIRY (0x12) command to retrieve device identity.
    pub unsafe fn inquiry(&mut self, xhci: &mut XhciDevice, scratch_pa: u64, scratch_va: u64) -> Result<(), &'static str> {
        let cdb = [SCSI_CMD_INQUIRY, 0, 0, 0, 36, 0];
        ptr::write_bytes(scratch_va as *mut u8, 0, 36);
        clean_dcache_range(scratch_va, 36);

        self.exec_scsi_cmd(xhci, &cdb, true, scratch_pa, scratch_va, 36)?;
        invalidate_dcache_range(scratch_va, 36);

        let buf = core::slice::from_raw_parts(scratch_va as *const u8, 36);
        self.vendor_name.copy_from_slice(&buf[8..16]);
        self.product_name.copy_from_slice(&buf[16..32]);
        self.revision.copy_from_slice(&buf[32..36]);
        Ok(())
    }

    /// Issue SCSI TEST UNIT READY (0x00) with optional retry loop.
    pub unsafe fn test_unit_ready(&mut self, xhci: &mut XhciDevice, scratch_pa: u64, scratch_va: u64) -> Result<(), &'static str> {
        let cdb = [SCSI_CMD_TEST_UNIT_READY, 0, 0, 0, 0, 0];
        for _ in 0..5 {
            if self.exec_scsi_cmd(xhci, &cdb, false, 0, 0, 0).is_ok() {
                return Ok(());
            }
            let _ = self.request_sense(xhci, scratch_pa, scratch_va);
        }
        Err("USB_MSC_UNIT_NOT_READY")
    }

    /// Issue SCSI REQUEST SENSE (0x03) command.
    pub unsafe fn request_sense(&mut self, xhci: &mut XhciDevice, scratch_pa: u64, scratch_va: u64) -> Result<(), &'static str> {
        let cdb = [SCSI_CMD_REQUEST_SENSE, 0, 0, 0, 18, 0];
        ptr::write_bytes(scratch_va as *mut u8, 0, 18);
        clean_dcache_range(scratch_va, 18);

        self.exec_scsi_cmd(xhci, &cdb, true, scratch_pa, scratch_va, 18)?;
        Ok(())
    }

    /// Issue SCSI READ CAPACITY (10) (0x25) command to get total blocks and block size.
    pub unsafe fn read_capacity(&mut self, xhci: &mut XhciDevice, scratch_pa: u64, scratch_va: u64) -> Result<(), &'static str> {
        let cdb = [SCSI_CMD_READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        ptr::write_bytes(scratch_va as *mut u8, 0, 8);
        clean_dcache_range(scratch_va, 8);

        self.exec_scsi_cmd(xhci, &cdb, true, scratch_pa, scratch_va, 8)?;
        invalidate_dcache_range(scratch_va, 8);

        let buf = core::slice::from_raw_parts(scratch_va as *const u8, 8);
        let last_lba = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64;
        let block_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

        self.total_blocks = last_lba.saturating_add(1);
        self.block_size = if block_size == 0 { 512 } else { block_size };
        self.is_initialized = true;
        Ok(())
    }

    /// Issue SCSI READ (10) (0x28) command to read `count` blocks starting at `lba`.
    pub unsafe fn read_sectors(
        &mut self,
        xhci: &mut XhciDevice,
        lba: u32,
        count: u16,
        buf_pa: u64,
        buf_va: u64,
    ) -> Result<(), &'static str> {
        if !self.is_initialized || count == 0 {
            return Err("USB_MSC_NOT_READY");
        }
        let total_bytes = (count as usize) * (self.block_size as usize);
        let cdb = [
            SCSI_CMD_READ_10,
            0,
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            (count >> 8) as u8,
            count as u8,
            0,
        ];
        self.exec_scsi_cmd(xhci, &cdb, true, buf_pa, buf_va, total_bytes)
    }

    pub fn vendor_str(&self) -> &str {
        core::str::from_utf8(&self.vendor_name).unwrap_or("Unknown").trim()
    }

    pub fn product_str(&self) -> &str {
        core::str::from_utf8(&self.product_name).unwrap_or("Unknown").trim()
    }

    pub fn revision_str(&self) -> &str {
        core::str::from_utf8(&self.revision).unwrap_or("").trim()
    }
}

pub static mut GLOBAL_USB_MSC: Option<UsbMscDevice> = None;

#[allow(static_mut_refs)]
pub fn register_global_usb_msc(dev: UsbMscDevice) {
    unsafe {
        GLOBAL_USB_MSC = Some(dev);
    }
}

#[allow(static_mut_refs)]
pub fn get_global_usb_msc() -> Option<&'static mut UsbMscDevice> {
    unsafe { GLOBAL_USB_MSC.as_mut() }
}

#[allow(static_mut_refs)]
pub fn get_global_usb_msc_ref() -> Option<&'static UsbMscDevice> {
    unsafe { GLOBAL_USB_MSC.as_ref() }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn usb_msc_cbw_and_csw_size() {
        assert_eq!(core::mem::size_of::<CommandBlockWrapper>(), 31);
        assert_eq!(core::mem::size_of::<CommandStatusWrapper>(), 13);
    }

    #[test]
    fn usb_msc_cbw_construction() {
        let cdb = [SCSI_CMD_INQUIRY, 0, 0, 0, 36, 0];
        let cbw = CommandBlockWrapper::new(0x12345678, 36, true, &cdb);
        let sig = cbw.signature;
        let tag = cbw.tag;
        let dlen = cbw.data_transfer_len;
        let flags = cbw.flags;
        let clen = cbw.cdb_len;
        let c0 = cbw.cdb[0];
        let c4 = cbw.cdb[4];
        assert_eq!(sig, CBW_SIGNATURE);
        assert_eq!(tag, 0x12345678);
        assert_eq!(dlen, 36);
        assert_eq!(flags, 0x80);
        assert_eq!(clen, 6);
        assert_eq!(c0, SCSI_CMD_INQUIRY);
        assert_eq!(c4, 36);
    }

    #[test]
    fn usb_msc_read_capacity_endianness() {
        let mock_resp: [u8; 8] = [
            0x00, 0x0F, 0xFF, 0xFF, // last LBA = 1,048,575 (1048576 blocks)
            0x00, 0x00, 0x02, 0x00, // block size = 512
        ];
        let last_lba = u32::from_be_bytes([mock_resp[0], mock_resp[1], mock_resp[2], mock_resp[3]]) as u64;
        let block_size = u32::from_be_bytes([mock_resp[4], mock_resp[5], mock_resp[6], mock_resp[7]]);
        assert_eq!(last_lba, 1_048_575);
        assert_eq!(last_lba + 1, 1_048_576);
        assert_eq!(block_size, 512);
    }
}

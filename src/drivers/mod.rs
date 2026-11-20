//! Device drivers for Project Genesis.

pub mod fb;
pub mod pci;
pub mod virtio_blk;
pub mod virtio_gpu;
pub mod virtio_input;
pub mod virtio_net;
pub mod xhci;
pub mod usb_hid;
pub mod nvme;
pub mod usb_msc;

pub use fb::{
    global_fb_console_clear, global_fb_console_write, redraw_workspace_sidebar,
    register_global_fb_console, update_workspace_telemetry, write_u64, FbConsole, Framebuffer,
    WorkspaceTelemetry,
};
pub use pci::{
    format_pci_device, pci_class_description, scan_bus_0, PciCapability, PciDeviceInfo,
    PCI_BAR_VA_BASE, PCI_DEVICES, PCI_DEVICE_COUNT, PCI_ECAM_DEFAULT_BASE, PCI_ECAM_VA_BASE,
};
pub use virtio_blk::{VirtIOBlockDevice, VIRTIO_DEV_BLOCK, VIRTIO_MAGIC, VIRTIO_MMIO_BASE};
pub use virtio_gpu::{get_global_virtio_gpu, register_global_virtio_gpu, VirtIOGpuDevice};
pub use virtio_input::{VirtIOInputDevice, VirtioInputEvent, VIRTIO_DEV_INPUT};
pub use virtio_net::{VirtIONetDevice, VIRTIO_DEV_NET};
pub use xhci::{
    format_port_status, format_xhci_info, get_global_xhci, get_global_xhci_ref,
    port_speed_str, register_global_xhci, ErstEntry, Trb, UsbPortInfo, XhciDevice,
    XHCI_MMIO_VA_BASE,
};
pub use nvme::{get_global_nvme, get_global_nvme_ref, register_global_nvme, NvmeDevice, NVME_MMIO_VA_BASE};
pub use usb_msc::{get_global_usb_msc, get_global_usb_msc_ref, register_global_usb_msc, UsbMscDevice};

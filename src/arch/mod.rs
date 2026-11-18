//! Architecture-specific primitives, exception handling, and drivers for Project Genesis (AArch64).

pub mod exceptions;
pub mod fdt;
pub mod gic;
pub mod serial;
pub mod timer;

pub use exceptions::{
    stage4_arm_timer_interval, stage4_get_ticks, stage4_init_runtime, stage4_vectors_va,
    TrapFrame,
};
pub use fdt::{parse_fdt_devices, FdtGicInfo, FdtTimerInfo};
pub use gic::{acknowledge_irq, end_of_irq, Gicv3};
pub use serial::{mmio_read, mmio_write, serial_decimal, serial_hex, serial_write};
pub use timer::ArmTimer;

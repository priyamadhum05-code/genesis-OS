//! Architecture-specific primitives, exception handling, and drivers for Project Genesis (AArch64).

pub mod acpi;
pub mod cpu;
pub mod exceptions;
pub mod fdt;
pub mod gic;
pub mod aic;
pub mod psci;
pub mod rtc;
pub mod serial;
pub mod timer;
pub mod m1n1;

pub use aic::{AppleInterruptController, AIC_INFO, AIC_WHOAMI, AIC_IACK, AIC_MASK_SET, AIC_MASK_CLR};
pub use exceptions::{
    stage4_arm_timer_interval, stage4_arm_timer_ticks, stage4_get_ticks, stage4_init_runtime,
    stage4_vectors_va, TrapFrame,
};
pub use fdt::{
    parse_fdt_aic, parse_fdt_apple_uart, parse_fdt_devices, parse_fdt_pci, parse_fdt_simplefb,
    FdtAicInfo, FdtGicInfo, FdtPciInfo, FdtSimpleFbInfo, FdtTimerInfo, FdtUartInfo,
};
pub use gic::{acknowledge_irq, end_of_irq, Gicv3};
pub use psci::{
    dispatch_smp_work, psci_cpu_on, psci_system_off, psci_system_reset, secondary_boot_stub, secondary_main,
    SecondaryBootArgs, ONLINE_CPUS, PSCI_SYSTEM_OFF, PSCI_SYSTEM_RESET, SMP_WORK_DONE, SMP_WORK_READY,
};
pub use rtc::{
    calendar_to_epoch, current_utc_epoch_secs, days_in_month, epoch_to_calendar, format_time_hhmmss,
    format_utc_datetime, init_rtc, is_leap_year, BOOT_EPOCH_SECS,
};
pub use serial::{
    mmio_read, mmio_write, serial_decimal, serial_hex, serial_read_byte, serial_read_line,
    serial_try_read_byte, serial_write,
};
pub use timer::ArmTimer;

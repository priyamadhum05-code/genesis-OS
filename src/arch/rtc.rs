//! Project Genesis — Wall-Clock Real-Time Clock (RTC) & Calendar Services.
//!
//! Provides UTC epoch <-> calendar conversions (handling leap years), live RTC tracking
//! synchronized from boot firmware / PL031 RTC, and formatted date/time strings.

use core::sync::atomic::{AtomicU64, Ordering};

pub static BOOT_EPOCH_SECS: AtomicU64 = AtomicU64::new(0);
pub static BOOT_CNTVCT: AtomicU64 = AtomicU64::new(0);
pub static CNTFRQ: AtomicU64 = AtomicU64::new(62_500_000);

/// Baseline epoch seconds (2026-01-01 00:00:00 UTC) used if firmware RTC is uninitialized.
pub const BASELINE_EPOCH_SECS: u64 = 1_767_225_600;

/// Days in each month for a non-leap year.
const DAYS_PER_MONTH: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Returns true if `year` is a Gregorian leap year.
#[inline]
pub const fn is_leap_year(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Returns the number of days in the specified year and month (1..=12).
#[inline]
pub const fn days_in_month(year: u16, month: u8) -> u8 {
    if month < 1 || month > 12 {
        return 0;
    }
    if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS_PER_MONTH[(month - 1) as usize]
    }
}

/// Converts a calendar UTC date and time into seconds since Unix epoch (1970-01-01 00:00:00 UTC).
pub fn calendar_to_epoch(year: u16, month: u8, day: u8, hour: u8, min: u8, sec: u8) -> u64 {
    if year < 1970 || month < 1 || month > 12 || day < 1 {
        return 0;
    }

    let mut days = 0u64;

    // Accumulate whole years since 1970
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }

    // Accumulate whole months in current year
    for m in 1..month {
        days += days_in_month(year, m) as u64;
    }

    // Accumulate whole days in current month
    days += (day.saturating_sub(1)) as u64;

    days * 86400 + (hour as u64) * 3600 + (min as u64) * 60 + (sec as u64)
}

/// Converts seconds since Unix epoch into calendar components `(year, month, day, hour, min, sec)`.
pub fn epoch_to_calendar(epoch_secs: u64) -> (u16, u8, u8, u8, u8, u8) {
    let mut total_days = epoch_secs / 86400;
    let rem_secs = epoch_secs % 86400;

    let hour = (rem_secs / 3600) as u8;
    let min = ((rem_secs % 3600) / 60) as u8;
    let sec = (rem_secs % 60) as u8;

    let mut year = 1970u16;
    loop {
        let days_this_year = if is_leap_year(year) { 366 } else { 365 };
        if total_days < days_this_year {
            break;
        }
        total_days -= days_this_year;
        year += 1;
    }

    let mut month = 1u8;
    while month <= 12 {
        let dim = days_in_month(year, month) as u64;
        if total_days < dim {
            break;
        }
        total_days -= dim;
        month += 1;
    }

    let day = (total_days + 1) as u8;
    (year, month, day, hour, min, sec)
}

/// Formats `epoch_secs` as `"YYYY-MM-DD HH:MM:SS UTC"` into `buf` and returns the string slice.
pub fn format_utc_datetime<'a>(epoch_secs: u64, buf: &'a mut [u8]) -> &'a str {
    if buf.len() < 24 {
        return "";
    }

    let (y, m, d, hh, mm, ss) = epoch_to_calendar(epoch_secs);

    buf[0] = b'0' + ((y / 1000) % 10) as u8;
    buf[1] = b'0' + ((y / 100) % 10) as u8;
    buf[2] = b'0' + ((y / 10) % 10) as u8;
    buf[3] = b'0' + (y % 10) as u8;
    buf[4] = b'-';
    buf[5] = b'0' + (m / 10);
    buf[6] = b'0' + (m % 10);
    buf[7] = b'-';
    buf[8] = b'0' + (d / 10);
    buf[9] = b'0' + (d % 10);
    buf[10] = b' ';
    buf[11] = b'0' + (hh / 10);
    buf[12] = b'0' + (hh % 10);
    buf[13] = b':';
    buf[14] = b'0' + (mm / 10);
    buf[15] = b'0' + (mm % 10);
    buf[16] = b':';
    buf[17] = b'0' + (ss / 10);
    buf[18] = b'0' + (ss % 10);
    buf[19] = b' ';
    buf[20] = b'U';
    buf[21] = b'T';
    buf[22] = b'C';

    core::str::from_utf8(&buf[..23]).unwrap_or("")
}

/// Formats `epoch_secs` as `"HH:MM:SS"` into `buf` and returns the string slice.
pub fn format_time_hhmmss<'a>(epoch_secs: u64, buf: &'a mut [u8]) -> &'a str {
    if buf.len() < 8 {
        return "";
    }

    let (_, _, _, hh, mm, ss) = epoch_to_calendar(epoch_secs);

    buf[0] = b'0' + (hh / 10);
    buf[1] = b'0' + (hh % 10);
    buf[2] = b':';
    buf[3] = b'0' + (mm / 10);
    buf[4] = b'0' + (mm % 10);
    buf[5] = b':';
    buf[6] = b'0' + (ss / 10);
    buf[7] = b'0' + (ss % 10);

    core::str::from_utf8(&buf[..8]).unwrap_or("")
}

/// Initializes system RTC tracking using `boot_epoch_secs` from bootloader/firmware.
pub fn init_rtc(boot_epoch_secs: u64) {
    let effective_epoch = if boot_epoch_secs >= 1_700_000_000 {
        boot_epoch_secs
    } else {
        BASELINE_EPOCH_SECS
    };

    #[allow(unused_mut)]
    let mut cntvct = 0u64;
    #[allow(unused_mut)]
    let mut cntfrq = 62_500_000u64;

    #[cfg(target_os = "uefi")]
    unsafe {
        core::arch::asm!("mrs {0}, cntvct_el0", out(reg) cntvct, options(nomem, nostack));
        core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));
    }

    if cntfrq == 0 {
        cntfrq = 62_500_000;
    }

    BOOT_EPOCH_SECS.store(effective_epoch, Ordering::Relaxed);
    BOOT_CNTVCT.store(cntvct, Ordering::Relaxed);
    CNTFRQ.store(cntfrq, Ordering::Relaxed);
}

/// Returns the current UTC epoch seconds calculated from boot RTC + elapsed ARM generic timer ticks.
pub fn current_utc_epoch_secs() -> u64 {
    let boot_epoch = BOOT_EPOCH_SECS.load(Ordering::Relaxed);
    let boot_vct = BOOT_CNTVCT.load(Ordering::Relaxed);
    let freq = CNTFRQ.load(Ordering::Relaxed);

    #[allow(unused_mut)]
    let mut current_vct = boot_vct;
    #[cfg(target_os = "uefi")]
    unsafe {
        core::arch::asm!("mrs {0}, cntvct_el0", out(reg) current_vct, options(nomem, nostack));
    }

    let elapsed_ticks = current_vct.saturating_sub(boot_vct);
    let elapsed_secs = if freq > 0 { elapsed_ticks / freq } else { 0 };

    boot_epoch + elapsed_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leap_year_rules() {
        assert!(!is_leap_year(1970));
        assert!(!is_leap_year(1971));
        assert!(is_leap_year(1972));
        assert!(is_leap_year(2000)); // divisible by 400
        assert!(!is_leap_year(1900)); // divisible by 100, not 400
        assert!(!is_leap_year(2100)); // divisible by 100, not 400
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2025));
        assert!(!is_leap_year(2026));
        assert!(is_leap_year(2028));
    }

    #[test]
    fn test_days_in_month() {
        assert_eq!(days_in_month(2024, 2), 29); // leap
        assert_eq!(days_in_month(2025, 2), 28); // non-leap
        assert_eq!(days_in_month(2026, 1), 31);
        assert_eq!(days_in_month(2026, 4), 30);
    }

    #[test]
    fn test_calendar_to_epoch_and_roundtrip() {
        // Unix epoch start: 1970-01-01 00:00:00
        assert_eq!(calendar_to_epoch(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(epoch_to_calendar(0), (1970, 1, 1, 0, 0, 0));

        // Y2K: 2000-01-01 00:00:00
        let y2k = calendar_to_epoch(2000, 1, 1, 0, 0, 0);
        assert_eq!(y2k, 946_684_800);
        assert_eq!(epoch_to_calendar(y2k), (2000, 1, 1, 0, 0, 0));

        // Leap day: 2024-02-29 12:34:56
        let leap_day = calendar_to_epoch(2024, 2, 29, 12, 34, 56);
        assert_eq!(epoch_to_calendar(leap_day), (2024, 2, 29, 12, 34, 56));

        // Date in 2026: 2026-11-22 20:45:30
        let date_2026 = calendar_to_epoch(2026, 11, 22, 20, 45, 30);
        assert_eq!(epoch_to_calendar(date_2026), (2026, 11, 22, 20, 45, 30));
    }

    #[test]
    fn test_format_datetime_strings() {
        let ts = calendar_to_epoch(2026, 11, 22, 14, 30, 45);
        let mut buf = [0u8; 32];
        let dt_str = format_utc_datetime(ts, &mut buf);
        assert_eq!(dt_str, "2026-11-22 14:30:45 UTC");

        let mut time_buf = [0u8; 16];
        let time_str = format_time_hhmmss(ts, &mut time_buf);
        assert_eq!(time_str, "14:30:45");
    }
}

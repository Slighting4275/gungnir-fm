//! Local wall-clock formatting through `localtime_r` (respects `TZ` and
//! `/etc/localtime`), without pulling in a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Broken-down local time: (year, month 1-12, day, hour, minute, second).
#[must_use]
pub fn local_parts(t: SystemTime) -> Option<(i32, u32, u32, u32, u32, u32)> {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => libc::time_t::try_from(d.as_secs()).ok()?,
        Err(e) => -libc::time_t::try_from(e.duration().as_secs()).ok()?,
    };
    // SAFETY: `tm` is plain old data and `localtime_r` writes it fully on success.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the duration of the call.
    if unsafe { libc::localtime_r(&raw const secs, &raw mut tm) }.is_null() {
        return None;
    }
    let u = |v: libc::c_int| u32::try_from(v).ok();
    Some((tm.tm_year + 1900, u(tm.tm_mon + 1)?, u(tm.tm_mday)?, u(tm.tm_hour)?, u(tm.tm_min)?, u(tm.tm_sec)?))
}

/// `2026-10-24 13:05`, or an empty string for an unknown time.
#[must_use]
pub fn format_local(t: Option<SystemTime>) -> String {
    t.and_then(local_parts)
        .map(|(y, mo, d, h, mi, _)| format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}"))
        .unwrap_or_default()
}

/// `2026-10-24T13:05:09`, the Trash spec's `DeletionDate` format.
#[must_use]
pub fn trash_date(t: SystemTime) -> String {
    local_parts(t)
        .map(|(y, mo, d, h, mi, s)| format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"))
        .unwrap_or_default()
}

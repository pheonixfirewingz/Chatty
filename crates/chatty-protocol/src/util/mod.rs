//! Small shared helpers used by the broker and clients.

pub mod args;
pub mod base64;
mod errors;
pub mod ids;
pub mod pemfile;

use std::time::{SystemTime, UNIX_EPOCH};

// The macros are declared with #[macro_export] at the crate root; re-export
// them here so callers can `use chatty_protocol::util::{bail, format_err}`.
pub use errors::{Context, Error, Result};
pub use crate::bail;
pub use crate::format_err;
pub use ids::new_uuid;

pub fn current_utc_timestamp() -> String {
    format_utc_timestamp(SystemTime::now())
}

/// Formats an instant as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// `std` offers no calendar formatting, so this converts a Unix timestamp to
/// a civil date directly (Howard Hinnant's `civil_from_days` algorithm).
pub(crate) fn format_utc_timestamp(timestamp: SystemTime) -> String {
    let Ok(elapsed) = timestamp.duration_since(UNIX_EPOCH) else {
        return "unknown time UTC".to_owned();
    };
    let total_seconds = elapsed.as_secs() as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60,
    )
}

/// Converts days since 1970-01-01 into a proleptic Gregorian `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to March 1st so leap days land at the end of a year,
    // then split time into 400-year eras where every era has the same shape.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((if m <= 2 { yoe + 1 } else { yoe }) + era * 400, m, d)
}

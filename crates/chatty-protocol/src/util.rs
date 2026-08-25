//! Timestamp formatting shared by broker and clients.

const UTC_TIMESTAMP_FORMAT: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");

pub fn current_utc_timestamp() -> String {
    format_utc_timestamp(time::OffsetDateTime::now_utc())
}

pub(crate) fn format_utc_timestamp(timestamp: time::OffsetDateTime) -> String {
    timestamp
        .format(UTC_TIMESTAMP_FORMAT)
        .unwrap_or_else(|_| "unknown time UTC".to_owned())
}

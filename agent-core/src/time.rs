//! ISO 时间、毫秒时间戳与本地时间格式化。

use chrono::SecondsFormat;

/// RFC3339 → 毫秒时间戳，解析失败返回 0。
pub fn iso_to_ms(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp_millis())
        .unwrap_or(0)
}

/// 毫秒时间戳 → RFC3339（毫秒精度，UTC），超出范围时回落到 Unix 纪元。
pub fn ms_to_iso(ms: i64) -> String {
    let fallback = chrono::DateTime::from_timestamp_millis(0).unwrap_or_default();
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or(fallback)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// 本地时区格式化，超出范围时返回占位符。
pub fn local_time(ms: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format(format)
                .to_string()
        })
        .unwrap_or_else(|| "-".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_helpers_roundtrip() {
        assert_eq!(iso_to_ms("2026-11-01T10:00:00.000Z"), 1_793_527_200_000);
        assert_eq!(
            ms_to_iso(iso_to_ms("2026-11-01T10:00:00.500Z")),
            "2026-11-01T10:00:00.500Z"
        );
        assert_eq!(iso_to_ms("not-a-time"), 0);
        assert_eq!(ms_to_iso(i64::MAX), "1970-01-01T00:00:00.000Z");
    }
}

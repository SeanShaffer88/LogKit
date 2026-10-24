//! LogKit — structured, rolling, crash-safe logging in one house
//! format, behind a small C ABI.
//!
//! The problem this solves (WinDevKit PAIN_POINTS #14): every app
//! reinvented logging in an incompatible format, so no tool, habit
//! or muscle memory transferred between projects — and when one
//! app failed to launch, there was no log to hunt with until a
//! milestone log got bolted on after the fact. LogKit's rules:
//!
//! - **One line, one record, one format — everywhere.**
//!   `2026-10-23T19:50:43.123Z INFO  net: connected host=example.com attempt=2`
//!   UTC ISO-8601 with milliseconds, a fixed-width level, a target
//!   (module/subsystem), the message, then structured `key=value`
//!   fields. Human-readable in Notepad, machine-parseable with a
//!   split — the same shape in every app, forever.
//! - **Crash-safe by construction.** Every record is flushed the
//!   moment it's written, so a crash loses at most the record
//!   being written. A record never spans two files: rotation
//!   happens *before* a write that would overflow, never mid-line.
//! - **Rolling with a hard budget.** Size-based rotation keeps the
//!   current file plus N backups (`app.log`, `app.log.1`, …) — a
//!   log that can't eat the disk it shares with the user's data.
//! - **There from the first line.** Opening a logger is one call
//!   with plain parameters; there's nothing to configure before
//!   the first milestone can be written.
//!
//! The clock, formatting and field handling live here and in
//! `fields`; the rolling file sink lives in `platform`; the C ABI
//! lives in `ffi`.

pub mod ffi;
pub mod fields;
pub mod platform;

pub const VERSION: &str = "0.1.0";
pub const VERSION_MAJOR: u32 = 0;
pub const VERSION_MINOR: u32 = 1;
pub const VERSION_PATCH: u32 = 0;

// Error codes (mirror include/logkit.h).
pub const LK_OK: i32 = 0;
pub const ERR_INVALID_ARG: i32 = 1;
pub const ERR_PARSE: i32 = 2;
pub const ERR_IO: i32 = 3;
pub const ERR_INTERNAL: i32 = 4;

// Levels (mirror include/logkit.h). Higher = more severe.
pub const LEVEL_TRACE: i32 = 1;
pub const LEVEL_DEBUG: i32 = 2;
pub const LEVEL_INFO: i32 = 3;
pub const LEVEL_WARN: i32 = 4;
pub const LEVEL_ERROR: i32 = 5;

pub type LkResult<T> = Result<T, (i32, String)>;

pub fn level_name(level: i32) -> Option<&'static str> {
    match level {
        LEVEL_TRACE => Some("TRACE"),
        LEVEL_DEBUG => Some("DEBUG"),
        LEVEL_INFO => Some("INFO"),
        LEVEL_WARN => Some("WARN"),
        LEVEL_ERROR => Some("ERROR"),
        _ => None,
    }
}

pub fn valid_level(level: i32) -> bool {
    level_name(level).is_some()
}

/// Broken-down UTC civil time for one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

/// Convert milliseconds since the Unix epoch to UTC civil time.
/// (Howard Hinnant's civil-from-days algorithm; correct across
/// leap years and for negative inputs.)
pub fn civil_from_unix_millis(ms: i64) -> Civil {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let hour = (rem / 3_600_000) as u32;
    let minute = ((rem % 3_600_000) / 60_000) as u32;
    let second = ((rem % 60_000) / 1000) as u32;
    let millis = (rem % 1000) as u32;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    Civil {
        year,
        month,
        day,
        hour,
        minute,
        second,
        millis,
    }
}

/// `2026-10-23T19:50:43.123Z` — the one timestamp shape.
pub fn format_timestamp(ms: i64) -> String {
    let c = civil_from_unix_millis(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis
    )
}

/// Escape a message onto one physical line: embedded newlines and
/// carriage returns become the two-character sequences `\n` / `\r`
/// so one record is always exactly one line in the file.
pub fn one_line(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for ch in message.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Format one record (without the trailing newline). Field values
/// render per `fields::render_value`: bare when safe, quoted with
/// escapes otherwise.
pub fn format_record(
    ms: i64,
    level: i32,
    target: &str,
    message: &str,
    fields: &[(String, fields::FieldValue)],
) -> LkResult<String> {
    let name = level_name(level).ok_or((
        ERR_INVALID_ARG,
        format!("invalid level {level} (expected 1..=5)"),
    ))?;
    let mut out = String::with_capacity(64 + target.len() + message.len());
    out.push_str(&format_timestamp(ms));
    out.push(' ');
    out.push_str(name);
    // Fixed-width level column: pad to 5 so messages align.
    for _ in name.len()..5 {
        out.push(' ');
    }
    out.push(' ');
    out.push_str(&one_line(target));
    out.push_str(": ");
    out.push_str(&one_line(message));
    for (k, v) in fields {
        out.push(' ');
        out.push_str(k);
        out.push('=');
        out.push_str(&fields::render_value(v));
    }
    Ok(out)
}

/// Escape a string for inclusion inside JSON double quotes
/// (kept for header/ABI parity with the family skeleton).
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00.000Z");
        // 2026-10-23T19:50:43.123Z (millis cross-checked with `date`).
        assert_eq!(
            format_timestamp(1_792_785_043_123),
            "2026-10-23T19:50:43.123Z"
        );
        // Leap day: 2024-02-29T12:00:00Z.
        assert_eq!(
            format_timestamp(1_709_208_000_000),
            "2024-02-29T12:00:00.000Z"
        );
        // Pre-epoch instant stays correct.
        assert_eq!(format_timestamp(-1), "1969-12-31T23:59:59.999Z");
        // Year boundary.
        assert_eq!(
            format_timestamp(1_767_225_600_000),
            "2026-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn record_shape() {
        let fields = vec![
            (
                "host".to_string(),
                fields::FieldValue::Str("example.com".into()),
            ),
            ("attempt".to_string(), fields::FieldValue::I64(2)),
        ];
        let line = format_record(0, LEVEL_INFO, "net", "connected", &fields).unwrap();
        assert_eq!(
            line,
            "1970-01-01T00:00:00.000Z INFO  net: connected host=example.com attempt=2"
        );
        let warn = format_record(0, LEVEL_WARN, "disk", "low", &[]).unwrap();
        assert!(warn.contains("WARN  disk: low"), "{warn}");
        let err_line = format_record(0, LEVEL_ERROR, "disk", "gone", &[]).unwrap();
        assert!(err_line.contains("ERROR disk: gone"), "{err_line}");
        assert_eq!(
            format_record(0, 99, "x", "y", &[]).unwrap_err().0,
            ERR_INVALID_ARG
        );
    }

    #[test]
    fn messages_stay_one_line() {
        let line = format_record(0, LEVEL_INFO, "app", "first\nsecond\r\nthird", &[]).unwrap();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains("first\\nsecond"), "{line}");
    }

    #[test]
    fn escapes_json() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}

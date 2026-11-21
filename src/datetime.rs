use jiff::{fmt::temporal::DateTimePrinter, tz::Offset, Timestamp};

pub fn parse(value: &str) -> Option<Timestamp> {
    value.trim().replacen(' ', "T", 1).parse().ok()
}

pub fn rfc3339(value: Timestamp) -> String {
    let nanos = value.subsec_nanosecond();
    let precision = match nanos {
        0 => 0,
        nanos if nanos % 1_000_000 == 0 => 3,
        nanos if nanos % 1_000 == 0 => 6,
        _ => 9,
    };
    DateTimePrinter::new()
        .precision(Some(precision))
        .timestamp_with_offset_to_string(&value, Offset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_timestamp_formats_preserve_the_same_instant() {
        let expected = parse("2026-09-21T22:27:58.096Z").unwrap();
        for value in [
            "2026-09-21 22:27:58.096000+00:00",
            "2026-09-22 00:27:58.096+02:00",
            "2026-09-21 22:27:58.096000+0000",
        ] {
            assert_eq!(parse(value), Some(expected));
        }
        assert!(parse("2026-09-21T22:27:58").is_none());
        assert!(parse("never").is_none());
    }

    #[test]
    fn public_timestamps_keep_utc_offset_and_fractional_precision() {
        for (value, expected) in [
            ("2026-09-21T22:27:58Z", "2026-09-21T22:27:58+00:00"),
            ("2026-09-21T22:27:58.1Z", "2026-09-21T22:27:58.100+00:00"),
            (
                "2026-09-21T22:27:58.000001Z",
                "2026-09-21T22:27:58.000001+00:00",
            ),
            (
                "2026-09-21T22:27:58.000000001Z",
                "2026-09-21T22:27:58.000000001+00:00",
            ),
        ] {
            assert_eq!(rfc3339(parse(value).unwrap()), expected);
        }
    }
}

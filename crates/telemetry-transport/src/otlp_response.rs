//! Bounded OTLP/HTTP JSON success-response parsing shared by native and WASM.
use crate::{OtlpSignal, TransportError};
use serde_json::value::RawValue;
use std::collections::BTreeMap;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Read a signal's rejected-record count from an OTLP success response.
///
/// ProtoJSON int64 accepts integer-valued decimal/exponent notation, quoted or
/// unquoted, and treats null as unset. Raw numeric lexemes preserve tiny
/// fractions that floating-point JSON decoders would incorrectly round to zero.
/// Returned errors never include collector text or input data.
pub fn parse_otlp_response(
    body: &[u8],
    signal: OtlpSignal,
    submitted: usize,
) -> Result<usize, TransportError> {
    let invalid = || TransportError::permanent("invalid OTLP success response");
    if body.len() > MAX_RESPONSE_BYTES || submitted > 1024 {
        return Err(invalid());
    }
    let response: BTreeMap<String, &RawValue> =
        serde_json::from_slice(body).map_err(|_| invalid())?;
    let partial = match response.get("partialSuccess") {
        None => return Ok(0),
        Some(value) if value.get() == "null" => return Ok(0),
        Some(value) => serde_json::from_str::<BTreeMap<String, &RawValue>>(value.get())
            .map_err(|_| invalid())?,
    };
    if let Some(message) = partial.get("errorMessage") {
        if message.get() != "null" && serde_json::from_str::<String>(message.get()).is_err() {
            return Err(invalid());
        }
    }
    let count = match partial.get(signal.rejected_field()) {
        None => return Ok(0),
        Some(value) if value.get() == "null" => return Ok(0),
        Some(value) => value.get(),
    };
    let decoded;
    let count = if count.starts_with('"') {
        decoded = serde_json::from_str::<String>(count).map_err(|_| invalid())?;
        decoded.as_str()
    } else {
        count
    };
    parse_decimal_count(count, submitted).ok_or_else(invalid)
}

/// Parse a bounded ProtoJSON int64 count without floating-point rounding.
/// Decimal/exponent notation is legal, but a fractional value must never
/// underflow to zero or round to a different integer before validation.
fn parse_decimal_count(text: &str, maximum: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut position = 0;
    let negative = bytes.first() == Some(&b'-');
    if negative {
        position += 1;
    }
    let integer_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        position += 1;
    }
    if position == integer_start {
        return None;
    }
    let integer_end = position;
    let mut fractional_digits = 0;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let fractional_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        fractional_digits = position - fractional_start;
        if fractional_digits == 0 {
            return None;
        }
    }
    let mantissa_end = position;
    let mut exponent = 0_i64;
    if matches!(bytes.get(position), Some(b'e' | b'E')) {
        position += 1;
        let exponent_negative = bytes.get(position) == Some(&b'-');
        if matches!(bytes.get(position), Some(b'+' | b'-')) {
            position += 1;
        }
        let exponent_start = position;
        while let Some(byte) = bytes.get(position).filter(|byte| byte.is_ascii_digit()) {
            // Inputs are bounded at 64KiB. Beyond this displacement a nonzero
            // count is either fractional or larger than any accepted batch.
            exponent = (exponent * 10 + i64::from(*byte - b'0')).min(100_000);
            position += 1;
        }
        if position == exponent_start {
            return None;
        }
        if exponent_negative {
            exponent = -exponent;
        }
    }
    if position != bytes.len() {
        return None;
    }
    let mut digits = bytes[integer_start..integer_end].to_vec();
    if fractional_digits > 0 {
        digits.extend_from_slice(&bytes[integer_end + 1..mantissa_end]);
    }
    let Some(first_nonzero) = digits.iter().position(|byte| *byte != b'0') else {
        return Some(0);
    };
    if negative {
        return None;
    }
    let mut significant = &digits[first_nonzero..];
    let scale = exponent - i64::try_from(fractional_digits).ok()?;
    if scale < 0 {
        let removed = usize::try_from(-scale).ok()?;
        if removed >= significant.len()
            || significant[significant.len() - removed..]
                .iter()
                .any(|byte| *byte != b'0')
        {
            return None;
        }
        significant = &significant[..significant.len() - removed];
    }
    let appended_zeros = usize::try_from(scale.max(0)).ok()?;
    if significant.len().saturating_add(appended_zeros) > maximum.to_string().len() {
        return None;
    }
    let mut count = 0_usize;
    for digit in significant {
        count = count
            .checked_mul(10)?
            .checked_add(usize::from(digit - b'0'))?;
        if count > maximum {
            return None;
        }
    }
    for _ in 0..appended_zeros {
        count = count.checked_mul(10)?;
        if count > maximum {
            return None;
        }
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protojson_response_counts_accept_decimal_exponents_and_null_for_each_signal() {
        for signal in [OtlpSignal::Logs, OtlpSignal::Metrics, OtlpSignal::Traces] {
            for (encoded, expected) in [
                ("1", 1),
                ("1.0", 1),
                ("1e0", 1),
                ("1E+0", 1),
                (r#""1e0""#, 1),
                (r#""1.0""#, 1),
                (r#""0001""#, 1),
                ("1.5e1", 15),
                (r#""1.5e1""#, 15),
                ("null", 0),
                ("-0.0", 0),
                ("0e99999", 0),
            ] {
                let body = format!(
                    r#"{{"partialSuccess":{{"{}":{encoded},"errorMessage":null}}}}"#,
                    signal.rejected_field()
                );
                assert_eq!(
                    parse_otlp_response(body.as_bytes(), signal, 20).unwrap(),
                    expected,
                    "{body}"
                );
            }
            for body in [
                r#"{}"#,
                r#"{"partialSuccess":null}"#,
                r#"{"partialSuccess":{}}"#,
            ] {
                assert_eq!(parse_otlp_response(body.as_bytes(), signal, 20).unwrap(), 0);
            }
        }
    }

    #[test]
    fn numeric_lexemes_cannot_round_fractional_rejections_to_integer_counts() {
        for signal in [OtlpSignal::Logs, OtlpSignal::Metrics, OtlpSignal::Traces] {
            for encoded in [
                "1e-9999",
                r#""1e-9999""#,
                "-1e-9999",
                r#""-1e-9999""#,
                "1.000000000000000000000000000001",
                r#""1.000000000000000000000000000001""#,
                "1.999999999999999999999999999999",
                r#""1.999999999999999999999999999999""#,
                "-1",
                "21",
                "1e99999",
                r#""NaN""#,
                r#""Infinity""#,
                "true",
                "{}",
                "[]",
            ] {
                let body = format!(
                    r#"{{"partialSuccess":{{"{}":{encoded}}}}}"#,
                    signal.rejected_field()
                );
                assert!(
                    parse_otlp_response(body.as_bytes(), signal, 20).is_err(),
                    "{body}"
                );
            }
        }
    }

    #[test]
    fn response_objects_and_known_message_fields_are_strict_and_bounded() {
        for body in [
            "[]",
            "null",
            r#"{"partialSuccess":[]}"#,
            r#"{"partialSuccess":0}"#,
            r#"{"partialSuccess":{"errorMessage":1}}"#,
            r#"{"partialSuccess":{"errorMessage":false}}"#,
        ] {
            assert!(parse_otlp_response(body.as_bytes(), OtlpSignal::Logs, 1).is_err());
        }
        let oversized = format!(r#"{{"padding":"{}"}}"#, "x".repeat(MAX_RESPONSE_BYTES));
        assert!(parse_otlp_response(oversized.as_bytes(), OtlpSignal::Logs, 1).is_err());
        assert!(parse_otlp_response(b"{}", OtlpSignal::Logs, 1025).is_err());
        assert_eq!(
            parse_otlp_response(
                br#"{"unknown":1e99999,"partialSuccess":{"unknown":-1e-99999,"rejectedLogRecords":null}}"#,
                OtlpSignal::Logs,
                1
            ).unwrap(),
            0
        );
        // Match normal map semantics for duplicate JSON keys without a separate
        // lexical parser disagreeing with the semantic object representation.
        assert_eq!(
            parse_otlp_response(
                br#"{"partialSuccess":{"rejectedLogRecords":1e-9999,"rejectedLogRecords":1e0}}"#,
                OtlpSignal::Logs,
                2
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn protojson_counts_use_exact_decimal_arithmetic() {
        for (text, expected) in [
            ("0", 0),
            ("-0", 0),
            ("-0.00e-9999", 0),
            ("00", 0),
            ("1", 1),
            ("01", 1),
            ("1.0", 1),
            ("1e0", 1),
            ("1E+0", 1),
            ("10e-1", 1),
            ("0.001e3", 1),
            ("1.5e1", 15),
            ("1500e-2", 15),
            ("256", 256),
            ("2.56e2", 256),
        ] {
            assert_eq!(parse_decimal_count(text, 256), Some(expected), "{text}");
        }
        for text in [
            "",
            "+1",
            " 1",
            "1 ",
            "1.",
            ".1",
            "1e",
            "1e+",
            "1e-",
            "NaN",
            "Infinity",
            "0x01",
            "1.1",
            "-1",
            "257",
            "1e9999",
            "1e-9999",
            "-1e-9999",
            "0.00000000000000001",
            "1.000000000000000000000000000001",
            "1.99999999999999999999999",
        ] {
            assert_eq!(parse_decimal_count(text, 256), None, "{text}");
        }
    }
}

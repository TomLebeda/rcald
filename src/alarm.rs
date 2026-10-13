use anyhow::{bail, Context, Result};

use crate::model::AlarmSpec;

/// Parse an RFC 5545 DURATION value (e.g. "-P0DT15H0M0S", "-PT15M", "PT0S", "-P1W")
/// into signed seconds. Zero durations are accepted (fire at the base time).
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let body = s
        .strip_prefix('P')
        .with_context(|| format!("invalid duration '{s}': missing leading 'P'"))?;

    let mut total: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    let mut units = 0usize;
    for c in body.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' => {
                if in_time || !num.is_empty() {
                    bail!("invalid duration '{s}': misplaced 'T'");
                }
                in_time = true;
            }
            'W' | 'D' | 'H' | 'M' | 'S' => {
                if num.is_empty() {
                    bail!("invalid duration '{s}': missing number before '{c}'");
                }
                let n: i64 = num
                    .parse()
                    .with_context(|| format!("invalid duration '{s}': bad number"))?;
                num.clear();
                let mult: i64 = match c {
                    'W' if !in_time => 7 * 24 * 3600,
                    'D' if !in_time => 24 * 3600,
                    'H' if in_time => 3600,
                    'M' if in_time => 60,
                    'S' if in_time => 1,
                    _ => bail!("invalid duration '{s}': '{c}' in wrong section"),
                };
                total = n
                    .checked_mul(mult)
                    .and_then(|v| total.checked_add(v))
                    .with_context(|| format!("invalid duration '{s}': too large"))?;
                units += 1;
            }
            other => bail!("invalid duration '{s}': unexpected '{other}'"),
        }
    }
    if !num.is_empty() {
        bail!("invalid duration '{s}': trailing number without unit");
    }
    if units == 0 {
        bail!("invalid duration '{s}': no units");
    }
    Ok(if neg { -total } else { total })
}

/// Build an AlarmSpec from a VALARM TRIGGER property value + params.
pub fn spec_from_trigger(
    value: &str,
    params: Option<&Vec<(String, Vec<String>)>>,
    local_tz: chrono_tz::Tz,
) -> Result<AlarmSpec> {
    if param_is(params, "VALUE", "DATE-TIME") {
        let (dt, _) = crate::ics::parse_datetime(value, params, local_tz)?;
        return Ok(AlarmSpec::Absolute(dt));
    }
    let offset_secs = parse_duration(value)?;
    let to_end = param_is(params, "RELATED", "END");
    Ok(AlarmSpec::Relative {
        offset_secs,
        to_end,
    })
}

pub(crate) fn param_is(params: Option<&Vec<(String, Vec<String>)>>, name: &str, val: &str) -> bool {
    param_value(params, name).is_some_and(|v| v.eq_ignore_ascii_case(val))
}

pub(crate) fn param_value<'a>(
    params: Option<&'a Vec<(String, Vec<String>)>>,
    name: &str,
) -> Option<&'a str> {
    params?
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))?
        .1
        .first()
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_duration("-P0DT15H0M0S").unwrap(), -15 * 3600);
        assert_eq!(parse_duration("-PT15M").unwrap(), -900);
        assert_eq!(parse_duration("-PT1H30M").unwrap(), -(3600 + 1800));
        assert_eq!(parse_duration("PT0S").unwrap(), 0);
        assert_eq!(parse_duration("-P1W").unwrap(), -7 * 24 * 3600);
        assert_eq!(parse_duration("P1DT2H").unwrap(), 24 * 3600 + 7200);
        assert_eq!(
            parse_duration("-P2DT4H30M").unwrap(),
            -(2 * 86400 + 4 * 3600 + 1800)
        );
        assert_eq!(
            parse_duration("-P13DT15H0M0S").unwrap(),
            -(13 * 86400 + 15 * 3600)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("15m").is_err());
        assert!(parse_duration("PT").is_err());
        assert!(parse_duration("PTM").is_err());
        assert!(parse_duration("P15").is_err());
        assert!(parse_duration("P1Q").is_err());
        assert!(parse_duration("PT1W").is_err()); // W not allowed in time part
        assert!(parse_duration("P99999999999999999W").is_err()); // overflow
    }

    #[test]
    fn trigger_relative_and_absolute() {
        let tz = chrono_tz::Tz::Europe__Prague;
        let rel = spec_from_trigger("-PT15M", None, tz).unwrap();
        match rel {
            AlarmSpec::Relative {
                offset_secs,
                to_end,
            } => {
                assert_eq!(offset_secs, -900);
                assert!(!to_end);
            }
            _ => panic!("expected relative"),
        }

        let params = vec![("RELATED".to_string(), vec!["END".to_string()])];
        let rel = spec_from_trigger("PT10M", Some(&params), tz).unwrap();
        match rel {
            AlarmSpec::Relative { to_end, .. } => assert!(to_end),
            _ => panic!("expected relative"),
        }

        let params = vec![("VALUE".to_string(), vec!["DATE-TIME".to_string()])];
        let abs = spec_from_trigger("20270103T090000Z", Some(&params), tz).unwrap();
        assert!(matches!(abs, AlarmSpec::Absolute(_)));
    }
}

//! Wire invariants shared by worker adapters, independent of browser values.
pub fn request_id(value: f64) -> Result<u32, String> {
    if !value.is_finite() || value < 1.0 || value > u32::MAX as f64 || value.fract() != 0.0 {
        return Err("Invalid request id".into());
    }
    Ok(value as u32)
}

/// A reply carries exactly one success payload or error, including empty bytes.
pub fn reply<T>(payload: Option<T>, error: Option<String>) -> Result<Result<T, String>, String> {
    match (payload, error) {
        (Some(payload), None) => Ok(Ok(payload)),
        (None, Some(error)) => Ok(Err(error)),
        (None, None) => Err("Missing reply payload".into()),
        (Some(_), Some(_)) => Err("Reply contains both bytes and error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_ids_that_would_alias_another_request() {
        for invalid in [0.0, -1.0, 1.5, f64::NAN, f64::INFINITY, u32::MAX as f64 + 1.0] {
            assert!(request_id(invalid).is_err());
        }
        assert_eq!(request_id(1.0).unwrap(), 1);
        assert_eq!(request_id(u32::MAX as f64).unwrap(), u32::MAX);
    }
    #[test]
    fn distinguishes_empty_success_application_error_and_malformed_reply() {
        assert_eq!(reply(Some(Vec::<u8>::new()), None), Ok(Ok(vec![])));
        assert_eq!(reply::<Vec<u8>>(None, Some("cancelled".into())), Ok(Err("cancelled".into())));
        assert!(reply::<Vec<u8>>(None, None).is_err());
        assert!(reply(Some(vec![1]), Some("failed".into())).is_err());
    }
}

/// Millisecond timers must fit the wire format and cannot wrap or panic.
pub fn milliseconds(value: f64) -> Result<u32, String> {
    if !value.is_finite() || !(0.0..=u32::MAX as f64).contains(&value) {
        return Err("Invalid duration".into());
    }
    Ok(value as u32)
}
#[cfg(test)]
mod timer_tests {
    #[test]
    fn rejects_unbounded_and_negative_durations() {
        for value in [f64::NAN, f64::INFINITY, -1.0, u32::MAX as f64 + 1.0] {
            assert!(super::milliseconds(value).is_err());
        }
        assert_eq!(super::milliseconds(0.0), Ok(0));
        assert_eq!(super::milliseconds(25.5), Ok(25));
    }
}

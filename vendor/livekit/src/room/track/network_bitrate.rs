use libwebrtc::rtp_parameters::RtpEncodingParameters;

pub(crate) type EncodingLimits = Vec<Option<u64>>;

pub(crate) fn apply_limit(
    encodings: &mut [RtpEncodingParameters],
    original: &EncodingLimits,
    limit: Option<u64>,
) -> Result<(), &'static str> {
    if encodings.is_empty()
        || encodings.len() != original.len()
        || limit.is_some_and(|v| v < encodings.len() as u64)
    {
        return Err("invalid video bandwidth limit or changed encodings");
    }
    let fallback = limit.unwrap_or(1);
    let total = original
        .iter()
        .map(|max| max.unwrap_or(fallback))
        .fold(0_u64, u64::saturating_add)
        .max(1);
    let limit = limit.map(|limit| {
        if original.iter().all(Option::is_some) {
            limit.min(total)
        } else {
            limit
        }
    });
    for (encoding, maximum) in encodings.iter_mut().zip(original) {
        encoding.max_bitrate = match limit {
            Some(limit) => Some(
                (maximum.unwrap_or(fallback).saturating_mul(limit) / total)
                    .max(1)
                    .min(maximum.unwrap_or(u64::MAX)),
            ),
            None => *maximum,
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn congestion_limits_all_layers_without_enabling_dynacast_layers_and_restores() {
        let mut encodings = vec![RtpEncodingParameters::default(); 2];
        encodings[0].active = false;
        let original = vec![Some(100_000), Some(900_000)];
        apply_limit(&mut encodings, &original, Some(200_000)).unwrap();
        assert_eq!(encodings[0].max_bitrate, Some(20_000));
        assert_eq!(encodings[1].max_bitrate, Some(180_000));
        assert!(!encodings[0].active);
        apply_limit(&mut encodings, &original, None).unwrap();
        assert_eq!(encodings[0].max_bitrate, Some(100_000));
        assert_eq!(encodings[1].max_bitrate, Some(900_000));
        assert!(!encodings[0].active);
    }

    #[test]
    fn limits_never_raise_a_configured_bitrate_and_can_restore_unset_limits() {
        let mut encodings = vec![RtpEncodingParameters::default()];
        apply_limit(&mut encodings, &vec![Some(60_000)], Some(300_000)).unwrap();
        assert_eq!(encodings[0].max_bitrate, Some(60_000));
        apply_limit(&mut encodings, &vec![None], Some(300_000)).unwrap();
        assert_eq!(encodings[0].max_bitrate, Some(300_000));
        apply_limit(&mut encodings, &vec![None], None).unwrap();
        assert_eq!(encodings[0].max_bitrate, None);
        let mut mixed = vec![RtpEncodingParameters::default(); 2];
        apply_limit(&mut mixed, &vec![Some(60_000), None], Some(300_000)).unwrap();
        assert_eq!(mixed[0].max_bitrate, Some(50_000));
        assert_eq!(mixed[1].max_bitrate, Some(250_000));
        assert!(apply_limit(&mut encodings, &vec![], Some(100)).is_err());
        assert!(apply_limit(&mut encodings, &vec![None], Some(0)).is_err());
    }
}

//! Read the movie duration without starting a second decoder or trusting box lengths.
use anyhow::{Context, Result, ensure};

fn boxes(mut data: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> Result<()>) -> Result<()> {
    let mut count = 0;
    while !data.is_empty() {
        count += 1;
        ensure!(
            count <= 65_536 && data.len() >= 8,
            "invalid/excessive MP4 boxes"
        );
        let length = u32::from_be_bytes(data[..4].try_into()?) as u64;
        let (length, header) = if length == 1 {
            ensure!(data.len() >= 16, "truncated extended MP4 box");
            (u64::from_be_bytes(data[8..16].try_into()?), 16)
        } else if length == 0 {
            (data.len() as u64, 8)
        } else {
            (length, 8)
        };
        ensure!(
            length >= header as u64 && length <= data.len() as u64,
            "MP4 box outside payload"
        );
        visit(&data[4..8], &data[header..length as usize])?;
        data = &data[length as usize..];
    }
    Ok(())
}
pub(super) fn duration(data: &[u8]) -> Result<Option<f64>> {
    let mut duration = None;
    boxes(data, |kind, payload| {
        if kind == b"moov" {
            boxes(payload, |kind, payload| {
                if kind == b"mvhd" {
                    ensure!(duration.is_none(), "duplicate MP4 movie header");
                    let version = *payload.first().context("empty MP4 movie header")?;
                    let (scale_at, duration_at, end) = match version {
                        0 => (12, 16, 20),
                        1 => (20, 24, 32),
                        _ => anyhow::bail!("unknown MP4 movie header version"),
                    };
                    ensure!(payload.len() >= end, "truncated MP4 movie duration");
                    let scale =
                        u32::from_be_bytes(payload[scale_at..duration_at].try_into()?) as f64;
                    let ticks = if version == 0 {
                        u32::from_be_bytes(payload[duration_at..end].try_into()?) as u64
                    } else {
                        u64::from_be_bytes(payload[duration_at..end].try_into()?)
                    };
                    if ticks
                        != if version == 0 {
                            u32::MAX as u64
                        } else {
                            u64::MAX
                        }
                        && scale > 0.
                        && ticks > 0
                    {
                        let seconds = ticks as f64 / scale;
                        ensure!(
                            seconds <= 31_536_000.,
                            "MP4 movie duration exceeds one year"
                        );
                        duration = Some(seconds);
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    })?;
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }
    #[test]
    fn versions_extended_boxes_unknown_duration_and_corrupt_bounds() {
        for version in [0, 1] {
            let mut header = vec![0; if version == 0 { 20 } else { 32 }];
            header[0] = version;
            let scale_at = if version == 0 { 12 } else { 20 };
            header[scale_at..scale_at + 4].copy_from_slice(&1000u32.to_be_bytes());
            if version == 0 {
                header[16..20].copy_from_slice(&2500u32.to_be_bytes());
            } else {
                header[24..32].copy_from_slice(&2500u64.to_be_bytes());
            }
            let movie = boxed(b"moov", &boxed(b"mvhd", &header));
            assert_eq!(duration(&movie).unwrap(), Some(2.5));
            let mut extended = 1u32.to_be_bytes().to_vec();
            extended.extend(b"moov");
            extended.extend((movie.len() as u64 + 8).to_be_bytes());
            extended.extend(&movie[8..]);
            assert_eq!(duration(&extended).unwrap(), Some(2.5));
            for length in [1, 7, movie.len() - 1] {
                assert!(duration(&movie[..length]).is_err());
            }
        }
        assert_eq!(duration(&boxed(b"ftyp", b"isom")).unwrap(), None);
        assert!(duration(&boxed(b"moov", &boxed(b"mvhd", &[4; 32]))).is_err());
    }
}

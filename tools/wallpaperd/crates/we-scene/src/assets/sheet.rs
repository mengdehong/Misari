//! Optional editor metadata for a static sprite atlas without a TEXS frame table.
use crate::assets::SpriteFrame;
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub(super) fn frames(value: &Value, content: [u32; 2]) -> Result<Vec<SpriteFrame>> {
    if value["spritesheetsequences"].is_null() {
        return Ok(Vec::new());
    }
    let sequences = value["spritesheetsequences"]
        .as_array()
        .context("invalid sprite sequences")?;
    ensure!(sequences.len() <= 64, "sprite sequence count exceeds limit");
    let mut frames = Vec::new();
    for (index, sequence) in sequences.iter().enumerate() {
        let sequence_frames = sequence_frames(sequence, content, index)?;
        ensure!(
            frames.len() + sequence_frames.len() <= 4096,
            "total sprite frame count exceeds limit"
        );
        frames.extend(sequence_frames);
    }
    Ok(frames)
}
fn sequence_frames(sequence: &Value, content: [u32; 2], index: usize) -> Result<Vec<SpriteFrame>> {
    let count = sequence["frames"]
        .as_u64()
        .context("invalid sprite frame count")?;
    ensure!(
        (1..=4096).contains(&count),
        "sprite frame count exceeds limit"
    );
    let number = |key: &str| -> Result<f64> {
        let value = sequence[key]
            .as_f64()
            .with_context(|| format!("sprite {key}"))?;
        ensure!(value.is_finite() && value > 0., "invalid sprite {key}");
        Ok(value)
    };
    let [width, height] = [number("width")?, number("height")?];
    let duration = if sequence["duration"].is_null() {
        1.
    } else {
        sequence["duration"].as_f64().context("sprite duration")?
    };
    ensure!(
        duration.is_finite() && duration > 0. && duration <= 3600.,
        "invalid sprite duration"
    );
    let frame_duration = (duration / count as f64) as f32;
    ensure!(
        frame_duration.is_finite() && frame_duration > 0.,
        "sprite frame duration underflows"
    );
    let grid = [content[0] as f64 / width, content[1] as f64 / height];
    ensure!(
        grid.iter().all(|n| n.is_finite()
            && (1. ..=4096.).contains(&n.round())
            && (n - n.round()).abs() <= 0.001),
        "sprite cells do not fit atlas"
    );
    let [columns, rows] = grid.map(|n| n.round() as u64);
    ensure!(count <= columns * rows, "sprite frames escape atlas");
    // Editor metadata rounds fractional cell dimensions (for example 1024/6).
    let [width, height] = [
        content[0] as f32 / columns as f32,
        content[1] as f32 / rows as f32,
    ];
    Ok((0..count)
        .map(|i| SpriteFrame {
            sequence: index,
            image: 0,
            duration: frame_duration,
            origin: [(i % columns) as f32 * width, (i / columns) as f32 * height],
            u: [width, 0.],
            v: [0., height],
            pixel_size: [width, height],
            ratio: height / width,
        })
        .collect())
}

pub(super) fn normalize(frames: &mut [SpriteFrame], atlas: [u32; 2]) {
    for frame in frames {
        frame.origin[0] /= atlas[0] as f32;
        frame.origin[1] /= atlas[1] as f32;
        frame.u[0] /= atlas[0] as f32;
        frame.u[1] /= atlas[1] as f32;
        frame.v[0] /= atlas[0] as f32;
        frame.v[1] /= atlas[1] as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn multiple_sequences_keep_independent_layout_timing_ranges_and_limits() {
        let value = json!({"spritesheetsequences":[{"frames":2,"width":1,"height":1,"duration":0.04},{"frames":2,"width":2,"height":1,"duration":1}]});
        let frames = super::frames(&value, [4, 1]).unwrap();
        assert_eq!(
            frames.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            [0, 0, 1, 1]
        );
        assert_eq!(crate::assets::sequence_range(&frames, 0), 0..2);
        assert_eq!(crate::assets::sequence_range(&frames, 1), 2..4);
        assert_eq!(crate::assets::sequence_range(&frames, 2), 4..4);
        assert_eq!(frames[0].duration, 0.02);
        assert_eq!(frames[2].duration, 0.5);
        assert_eq!(frames[1].origin, [1., 0.]);
        assert_eq!(frames[3].origin, [2., 0.]);
        assert_eq!(frames[2].pixel_size, [2., 1.]);
        for bad in [
            json!({"frames":5,"width":1,"height":1}),
            json!({"frames":1,"width":0,"height":1}),
            json!({"frames":1,"width":1,"height":1,"duration":1e-100}),
            json!({"frames":1,"width":1,"height":1,"duration":3601}),
        ] {
            let mut value = value.clone();
            value["spritesheetsequences"][1] = bad;
            assert!(super::frames(&value, [4, 1]).is_err());
        }
        assert!(
            super::frames(
                &json!({"spritesheetsequences":vec![value["spritesheetsequences"][0].clone();65]}),
                [4, 1]
            )
            .is_err()
        );
        assert!(super::frames(&json!({"spritesheetsequences":vec![json!({"frames":4096,"width":1,"height":1});2]}),[4096,1]).is_err());
    }
    #[test]
    fn fractional_cells_preserve_content_in_a_padded_atlas_and_validate_bounds() {
        let mut frames = frames(&json!({"spritesheetsequences":[{"frames":36,"width":170.6667,"height":170.666,"duration":2}]}), [1024,1024]).unwrap();
        assert_eq!(frames.len(), 36);
        assert!((frames[35].origin[0] - 1024. * 5. / 6.).abs() < 1e-4);
        assert!((frames.iter().map(|f| f.duration).sum::<f32>() - 2.).abs() < 1e-5);
        normalize(&mut frames, [2048, 1024]);
        assert!((frames[0].u[0] - 1. / 12.).abs() < 1e-6);
        assert!((frames[0].v[1] - 1. / 6.).abs() < 1e-6);
        assert_eq!(frames[0].ratio, 1.);
        for sequence in [
            json!({"frames":4097,"width":1,"height":1}),
            json!({"frames":5,"width":1,"height":1}),
            json!({"frames":1,"width":0,"height":1}),
            json!({"frames":1,"width":0.7,"height":1}),
            json!({"frames":1,"width":1,"height":1,"duration":-1}),
        ] {
            assert!(super::frames(&json!({"spritesheetsequences":[sequence]}), [2, 2]).is_err());
        }
    }
}

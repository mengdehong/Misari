use glam::{DMat3, DMat4, DVec3};
use rquickjs::{Ctx, Exception, Result};
pub(super) fn calculate(ctx: Ctx<'_>, op: String, n: u8, values: Vec<f64>) -> Result<Vec<f64>> {
    let expected = match op.as_str() {
        "rotation" => 4,
        "euler" => 3,
        "lookAt" => 9,
        _ => (n as usize).pow(2),
    };
    if ![3, 4].contains(&n) || values.len() != expected || values.iter().any(|v| !v.is_finite()) {
        return Err(Exception::throw_type(&ctx, "Invalid matrix input"));
    }
    if op == "rotation" {
        let axis = DVec3::new(values[1], values[2], values[3]);
        if axis.length_squared() < 1e-24 {
            return Err(Exception::throw_range(&ctx, "Rotation axis is zero"));
        }
        return Ok(
            DMat4::from_axis_angle(axis.normalize(), values[0].to_radians())
                .to_cols_array()
                .to_vec(),
        );
    }
    if op == "euler" {
        return Ok(DMat4::from_euler(
            crate::scene::EULER_ORDER,
            values[0].to_radians(),
            values[1].to_radians(),
            values[2].to_radians(),
        )
        .to_cols_array()
        .to_vec());
    }
    if op == "lookAt" {
        let eye = DVec3::from_slice(&values[..3]);
        let center = DVec3::from_slice(&values[3..6]);
        let up = DVec3::from_slice(&values[6..]);
        if (eye - center).cross(up).length_squared() < 1e-24 {
            return Err(Exception::throw_range(&ctx, "Degenerate camera basis"));
        }
        return Ok(DMat4::look_at_rh(eye, center, up.normalize())
            .to_cols_array()
            .to_vec());
    }
    let (determinant, inverse) = if n == 4 {
        let m = DMat4::from_cols_array(values.as_slice().try_into().unwrap());
        if op == "decompose" {
            let (scale, rotation, translation) = m.to_scale_rotation_translation();
            if !scale.is_finite() || scale.abs().min_element() < 1e-12 {
                return Err(Exception::throw_range(&ctx, "Singular transform"));
            }
            let (x, y, z) = rotation.to_euler(crate::scene::EULER_ORDER);
            return Ok([
                translation.to_array(),
                [x.to_degrees(), y.to_degrees(), z.to_degrees()],
                scale.to_array(),
            ]
            .concat());
        }
        (m.determinant(), m.inverse().to_cols_array().to_vec())
    } else {
        let m = DMat3::from_cols_array(values.as_slice().try_into().unwrap());
        (m.determinant(), m.inverse().to_cols_array().to_vec())
    };
    match op.as_str() {
        "determinant" => Ok(vec![determinant]),
        "inverse" if determinant.abs() > 1e-24 && inverse.iter().all(|v| v.is_finite()) => {
            Ok(inverse)
        }
        "inverse" => Err(Exception::throw_range(&ctx, "Matrix has no finite inverse")),
        _ => Err(Exception::throw_type(&ctx, "Unknown matrix operation")),
    }
}

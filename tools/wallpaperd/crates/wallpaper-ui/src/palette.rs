//! Wallpaper-derived accent, using the same Celebi → Score → HCT pipeline as Matugen.
use iced::{
    Border, Color, Shadow, Theme, Vector,
    widget::{container, image::Handle},
};
use material_colors::{
    color::Argb,
    hct::Hct,
    palette::TonalPalette,
    quantize::{Quantizer, QuantizerCelebi},
    score::Score,
};

const FALLBACK: Argb = Argb::new(255, 112, 132, 170);
pub const BG: Color = Color::from_rgb8(12, 14, 19);
pub const CANVAS: Color = Color::from_rgb8(19, 22, 29);
pub const PANEL: Color = Color::from_rgb8(25, 28, 35);
pub const CARD: Color = Color::from_rgb8(34, 38, 47);
// Midtone overlays keep GPU linear blending and the software fallback close.
pub const WASH: Color = Color::from_rgb8(113, 124, 146);
pub const LINE: Color = Color::from_rgba(0.37, 0.41, 0.50, 0.18);
pub const TEXT: Color = Color::from_rgb8(237, 240, 247);
pub const MUTED: Color = Color::from_rgb8(157, 166, 184);
pub const CONTROL_RADIUS: f32 = 11.0;
pub const SURFACE_RADIUS: f32 = 20.0;

pub fn canvas(_: &Theme) -> container::Style {
    container::Style {
        background: Some(CANVAS.into()),
        border: Border {
            color: WASH.scale_alpha(0.08),
            width: 1.0,
            radius: SURFACE_RADIUS.into(),
        },
        ..Default::default()
    }
}

/// Mica lets the blurred wallpaper light through the navigation and inspector.
pub fn mica(_: &Theme) -> container::Style {
    container::Style {
        background: Some(
            iced::gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, Color::from_rgba(0.12, 0.14, 0.18, 0.64))
                .add_stop(1.0, Color::from_rgba(0.065, 0.08, 0.11, 0.80))
                .into(),
        ),
        border: Border {
            color: WASH.scale_alpha(0.08),
            width: 1.0,
            radius: SURFACE_RADIUS.into(),
        },
        ..Default::default()
    }
}

/// The floating layer sits over the same preblurred backdrop: no per-frame blur.
pub fn glass(_: &Theme) -> container::Style {
    container::Style {
        background: Some(
            iced::gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, Color::from_rgba(0.30, 0.34, 0.42, 0.36))
                .add_stop(0.16, Color::from_rgba(0.16, 0.19, 0.25, 0.30))
                .add_stop(1.0, Color::from_rgba(0.08, 0.10, 0.15, 0.54))
                .into(),
        ),
        border: Border {
            color: WASH.scale_alpha(0.12),
            width: 1.0,
            radius: 24.0.into(),
        },
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(0.28),
            offset: Vector::new(0.0, 10.0),
            blur_radius: 30.0,
        },
        ..Default::default()
    }
}

/// Popovers need a denser tint to keep text readable over wallpaper thumbnails.
pub fn popover(theme: &Theme) -> container::Style {
    let mut style = glass(theme);
    style.background = Some(
        iced::gradient::Linear::new(std::f32::consts::PI)
            .add_stop(0.0, Color::from_rgba(0.16, 0.19, 0.25, 0.96))
            .add_stop(1.0, Color::from_rgba(0.075, 0.09, 0.12, 0.96))
            .into(),
    );
    style.border.radius = 16.0.into();
    style
}

/// Generated on the existing wallpaper-theme worker, only when its preview changes.
/// Blur a small image, then add fine grain after scaling to prevent colour banding.
pub fn ambient(handle: &Handle) -> Option<Handle> {
    let Handle::Rgba {
        width,
        height,
        pixels,
        ..
    } = handle
    else {
        return None;
    };
    let source = image::RgbaImage::from_raw(*width, *height, pixels.to_vec())?;
    let small = image::imageops::resize(&source, 96, 64, image::imageops::FilterType::Triangle);
    let blurred = image::imageops::blur(&small, 12.0);
    // Stay below Iced's 2 MiB synchronous upload limit so the first frame has its backdrop.
    let mut backdrop =
        image::imageops::resize(&blurred, 864, 576, image::imageops::FilterType::Triangle);
    for (x, y, pixel) in backdrop.enumerate_pixels_mut() {
        let u = x as f32 / 863.0 * 2.0 - 1.0;
        let v = y as f32 / 575.0 * 2.0 - 1.0;
        let opacity = (0.36 - 0.08 * (u * u + v * v)) * (pixel[3] as f32 / 255.0);
        let hash = (x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263)).wrapping_mul(1274126177);
        let grain = ((hash >> 24) as f32 / 255.0 - 0.5) * 3.0;
        for (channel, base) in [12.0, 14.0, 19.0].into_iter().enumerate() {
            pixel[channel] = (base * (1.0 - opacity) + pixel[channel] as f32 * opacity + grain)
                .clamp(0.0, 255.0) as u8;
        }
        pixel[3] = 255;
    }
    Some(Handle::from_rgba(864, 576, backdrop.into_raw()))
}

pub fn default_theme() -> Theme {
    from_source(FALLBACK)
}

fn from_source(source: Argb) -> Theme {
    let hct = Hct::new(source);
    let color = TonalPalette::of(hct.get_hue(), hct.get_chroma().clamp(24.0, 48.0)).tone(80);
    let accent = Color::from_rgb8(color.red, color.green, color.blue);
    Theme::custom(
        "Misari",
        iced::theme::Palette {
            background: BG,
            text: TEXT,
            primary: accent,
            success: accent,
            warning: Color::from_rgb8(219, 188, 141),
            danger: Color::from_rgb8(221, 153, 153),
        },
    )
}

pub fn from_preview(handle: Handle) -> Theme {
    let Handle::Rgba {
        width,
        height,
        pixels,
        ..
    } = handle
    else {
        return default_theme();
    };
    let Some(image) = image::RgbaImage::from_raw(width, height, pixels.to_vec()) else {
        return default_theme();
    };
    let image = image::imageops::thumbnail(&image, 112, 112);
    let pixels: Vec<_> = image
        .pixels()
        .filter(|pixel| pixel[3] == 255)
        .map(|pixel| Argb::new(255, pixel[0], pixel[1], pixel[2]))
        .collect();
    if pixels.is_empty() {
        return default_theme();
    }
    let colors = QuantizerCelebi::quantize(&pixels, 128);
    let source = Score::score(&colors.color_to_count, Some(1), Some(FALLBACK), None)[0];
    from_source(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_retains_wallpaper_light_without_detail_and_stays_dim() {
        let source = image::RgbaImage::from_fn(96, 64, |x, y| {
            let checker = if (x + y) % 2 == 0 { 80 } else { 0 };
            image::Rgba([
                if x < 48 { 160 + checker } else { checker },
                20,
                if x >= 48 { 160 + checker } else { checker },
                255,
            ])
        });
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = ambient(&Handle::from_rgba(96, 64, source.into_raw())).unwrap()
        else {
            panic!("RGBA ambient image");
        };
        let image = image::RgbaImage::from_raw(width, height, pixels.to_vec()).unwrap();
        assert!(
            width * height * 4 < 2 * 1024 * 1024,
            "the backdrop must upload on the first GPU frame"
        );
        let left = image.get_pixel(240, 320);
        let right = image.get_pixel(720, 320);
        assert!(left[0] > left[2] + 20 && right[2] > right[0] + 20);
        assert!(
            image
                .pixels()
                .all(|p| p[3] == 255 && p[0].max(p[1]).max(p[2]) < 100)
        );
        let neighbor = image.get_pixel(241, 320);
        assert!(
            left[0].abs_diff(neighbor[0]) <= 4,
            "fine source detail must be blurred away"
        );
        assert!(
            image
                .enumerate_pixels()
                .any(|(x, y, p)| x > 0 && p[1] != image.get_pixel(x - 1, y)[1]),
            "grain must survive the blur"
        );
        assert!(ambient(&Handle::from_bytes(vec![])).is_none());
    }

    #[test]
    fn wallpaper_hue_changes_without_sacrificing_button_contrast() {
        let theme = |rgb: [u8; 4]| from_preview(Handle::from_rgba(8, 8, rgb.repeat(64)));
        let warm = theme([220, 80, 35, 255]).palette().primary;
        let cool = theme([35, 100, 220, 255]).palette().primary;
        assert!(warm.r > warm.b);
        assert!(cool.b > cool.r);
        let luminance = |color: Color| {
            let [r, g, b, _] = color.into_linear();
            r * 0.2126 + g * 0.7152 + b * 0.0722
        };
        for accent in [warm, cool, default_theme().palette().primary] {
            assert!((luminance(accent) + 0.05) / (luminance(BG) + 0.05) > 7.0);
        }
        assert_eq!(
            theme([0, 0, 0, 0]).palette().primary,
            default_theme().palette().primary
        );
        assert_eq!(
            theme([128, 128, 128, 255]).palette().primary,
            default_theme().palette().primary
        );
    }
}

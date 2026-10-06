//! Decode only on demand, crop before filtering, and keep no full-resolution idle cache.
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use fast_image_resize::{
    PixelType, ResizeOptions, Resizer,
    images::{CroppedImageMut, Image, ImageRef},
};
use image::{ImageReader, Limits, RgbaImage};

use crate::{
    catalog,
    domain::{Fit, Selection},
};

const MAX_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        anyhow::ensure!(width > 0 && height > 0, "empty output");
        anyhow::ensure!(
            u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|n| n.checked_mul(4))
                .is_some_and(|n| n <= MAX_BYTES),
            "output buffer exceeds 512 MiB"
        );
        Ok(Self { width, height })
    }

    pub fn bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

pub fn decode(selection: &Selection) -> Result<RgbaImage> {
    decode_path(catalog::path(selection))
}

/// Content-addressed paths let shell texture caches notice new frames; PNG
/// encoding stays in the per-output worker, outside the session and shell UI.
pub fn cache_frame(image: &RgbaImage, cache: &Path) -> Result<PathBuf> {
    use image::{
        ImageEncoder,
        codecs::png::{CompressionType, FilterType, PngEncoder},
    };
    let mut hash = DefaultHasher::new();
    image.dimensions().hash(&mut hash);
    image.as_raw().hash(&mut hash);
    let path = cache.join(format!("{:016x}.png", hash.finish()));
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(cache)?;
    let temporary = cache.join(format!(
        ".{}-{:016x}.tmp",
        std::process::id(),
        hash.finish()
    ));
    let result = (|| -> Result<()> {
        let writer = fs::File::create(&temporary)?;
        PngEncoder::new_with_quality(writer, CompressionType::Fast, FilterType::Adaptive)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(path)
}

fn decode_path(path: &Path) -> Result<RgbaImage> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_BYTES);
    limits.max_image_width = Some(32768);
    limits.max_image_height = Some(32768);
    reader.limits(limits);
    let image = reader.decode().context("decoding wallpaper")?;
    Size::new(image.width(), image.height()).context("decoded image exceeds resource limit")?;
    Ok(image.into_rgba8())
}

/// Runs in the control client, never in the daemon's event loop or shell UI.
pub fn thumbnail(path: &Path, cache: &Path) -> Result<PathBuf> {
    thumbnail_with(path, cache, 1, || decode_path(path))
}

pub fn thumbnail_with(
    path: &Path,
    cache: &Path,
    version: u8,
    decode: impl FnOnce() -> Result<RgbaImage>,
) -> Result<PathBuf> {
    let metadata = fs::metadata(path)?;
    let mut hash = DefaultHasher::new();
    path.hash(&mut hash);
    metadata.len().hash(&mut hash);
    metadata.modified()?.hash(&mut hash);
    // Include the preview format version so older PNG-only caches expire.
    version.hash(&mut hash);
    let jpeg = cache.join(format!("{:016x}.jpg", hash.finish()));
    let png = jpeg.with_extension("png");
    for target in [&jpeg, &png] {
        if target.is_file() {
            return Ok(target.clone());
        }
    }
    let image = shrink_thumbnail(decode()?)?;
    let transparent = image.pixels().any(|pixel| pixel[3] != 255);
    let target = if transparent { png } else { jpeg };
    fs::create_dir_all(cache)?;
    let temporary = target.with_extension(format!("{}.tmp", std::process::id()));
    let result = if transparent {
        image.save_with_format(&temporary, image::ImageFormat::Png)
    } else {
        image::DynamicImage::ImageRgba8(image)
            .into_rgb8()
            .save_with_format(&temporary, image::ImageFormat::Jpeg)
    }
    .map_err(anyhow::Error::from)
    .and_then(|()| fs::rename(&temporary, &target).map_err(anyhow::Error::from));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(target)
}

pub fn shrink_thumbnail(source: RgbaImage) -> Result<RgbaImage> {
    Size::new(source.width(), source.height())?;
    let ratio = (640.0 / f64::from(source.width().max(source.height()))).min(1.0);
    if ratio == 1.0 {
        return Ok(source);
    }
    let size = Size::new(
        (f64::from(source.width()) * ratio).round().max(1.0) as u32,
        (f64::from(source.height()) * ratio).round().max(1.0) as u32,
    )?;
    let mut image = RgbaImage::new(size.width, size.height);
    render_rgba(source, size, Fit::Stretch, image.as_mut())?;
    Ok(image)
}

pub fn render(mut source: RgbaImage, size: Size, fit: Fit, bytes: &mut [u8]) -> Result<()> {
    anyhow::ensure!(source.width() > 0 && source.height() > 0, "empty image");
    // XRGB is a native-endian u32. Alpha was ignored by the original static backend.
    for pixel in source.as_mut().as_chunks_mut::<4>().0 {
        let value = (u32::from(pixel[0]) << 16)
            | (u32::from(pixel[1]) << 8)
            | u32::from(pixel[2])
            | 0xff00_0000;
        pixel.copy_from_slice(&value.to_ne_bytes());
    }
    render_rgba(source, size, fit, bytes)
}

pub fn render_rgba(source: RgbaImage, size: Size, fit: Fit, bytes: &mut [u8]) -> Result<()> {
    anyhow::ensure!(source.width() > 0 && source.height() > 0, "empty image");
    let input = ImageRef::new(
        source.width(),
        source.height(),
        source.as_raw(),
        PixelType::U8x4,
    )?;
    anyhow::ensure!(bytes.len() == size.bytes(), "incorrect canvas size");
    if fit == Fit::Contain {
        bytes.fill(0);
    }
    let mut output = Image::from_slice_u8(size.width, size.height, bytes, PixelType::U8x4)?;
    let options = ResizeOptions::new().use_alpha(false);
    // The resizer and its scratch buffers die with this task, rather than retaining the
    // largest wallpaper ever selected. SIMD is selected at runtime, without a thread pool.
    let mut resizer = Resizer::new();
    match fit {
        Fit::Cover => resizer.resize(&input, &mut output, &options.fit_into_destination(None))?,
        Fit::Stretch => resizer.resize(&input, &mut output, &options)?,
        Fit::Contain => {
            let ratio = (size.width as f64 / source.width() as f64)
                .min(size.height as f64 / source.height() as f64);
            let width = (source.width() as f64 * ratio)
                .round()
                .clamp(1.0, size.width as f64) as u32;
            let height = (source.height() as f64 * ratio)
                .round()
                .clamp(1.0, size.height as f64) as u32;
            let mut area = CroppedImageMut::new(
                &mut output,
                (size.width - width) / 2,
                (size.height - height) / 2,
                width,
                height,
            )?;
            resizer.resize(&input, &mut area, &options)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn thumbnails_are_small_cached_and_invalidated_by_source_changes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.png");
        let cache = root.path().join("cache");
        RgbaImage::from_pixel(1280, 640, Rgba([255, 0, 0, 255]))
            .save(&source)
            .unwrap();
        let first = thumbnail(&source, &cache).unwrap();
        assert_eq!(first.extension().unwrap(), "jpg");
        let decoded = image::open(&first).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (640, 320));
        let modified = fs::metadata(&first).unwrap().modified().unwrap();
        assert_eq!(thumbnail(&source, &cache).unwrap(), first);
        assert_eq!(fs::metadata(&first).unwrap().modified().unwrap(), modified);
        RgbaImage::from_pixel(32, 16, Rgba([0, 0, 255, 255]))
            .save(&source)
            .unwrap();
        let second = thumbnail(&source, &cache).unwrap();
        assert_ne!(second, first);
        assert_eq!(image::open(second).unwrap().width(), 32);
        RgbaImage::from_pixel(32, 16, Rgba([0, 255, 0, 128]))
            .save(&source)
            .unwrap();
        let transparent = thumbnail(&source, &cache).unwrap();
        assert_eq!(transparent.extension().unwrap(), "png");
        assert_eq!(
            image::open(transparent).unwrap().into_rgba8()[(0, 0)][3],
            128
        );
        assert!(thumbnail(&root.path().join("missing.png"), &cache).is_err());
    }

    struct Frame {
        size: Size,
        bytes: Vec<u8>,
    }
    fn rendered(source: RgbaImage, size: Size, fit: Fit) -> Result<Frame> {
        let mut bytes = vec![0; size.bytes()];
        render(source, size, fit, &mut bytes)?;
        Ok(Frame { size, bytes })
    }

    fn pixel(frame: &Frame, x: usize, y: usize) -> u32 {
        let offset = (y * frame.size.width as usize + x) * 4;
        u32::from_ne_bytes(frame.bytes[offset..offset + 4].try_into().unwrap()) & 0x00ff_ffff
    }

    #[test]
    fn fit_geometry_and_native_xrgb() {
        let source = RgbaImage::from_fn(4, 2, |x, _| {
            if x < 2 {
                Rgba([255, 0, 0, 0])
            } else {
                Rgba([0, 0, 255, 255])
            }
        });
        let cover = rendered(source.clone(), Size::new(2, 2).unwrap(), Fit::Cover).unwrap();
        assert_eq!(pixel(&cover, 0, 0), 0xff0000);
        assert_eq!(pixel(&cover, 1, 0), 0x0000ff);
        let contain = rendered(source.clone(), Size::new(2, 4).unwrap(), Fit::Contain).unwrap();
        assert_eq!(pixel(&contain, 0, 0), 0);
        assert_ne!(pixel(&contain, 0, 1), 0);
        let stretch = rendered(source, Size::new(2, 4).unwrap(), Fit::Stretch).unwrap();
        assert_ne!(pixel(&stretch, 0, 0), 0);
    }

    #[test]
    fn extreme_aspect_ratios_are_bounded_by_destination() {
        let source = RgbaImage::from_pixel(32768, 1, Rgba([3, 5, 7, 255]));
        let frame = rendered(source, Size::new(1, 4096).unwrap(), Fit::Cover).unwrap();
        assert_eq!(frame.bytes.len(), 4096 * 4);
        assert_eq!(pixel(&frame, 0, 2048), 0x030507);
        assert!(Size::new(u32::MAX, u32::MAX).is_err());
        assert!(Size::new(0, 1).is_err());
    }
}

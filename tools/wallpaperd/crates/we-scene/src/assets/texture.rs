//! TEX images and affine animation frames. Bounds are checked before GPU allocation.
use super::tex::{TexHeader, TexImage, TexMipmap, read_tex};
use super::{LIMIT, SpriteFrame, Texture, TextureInfo, TextureMip};
use anyhow::{Context, Result, ensure};
use std::{borrow::Cow, io::Cursor};

pub(super) fn metadata(data: &[u8]) -> Result<TextureInfo> {
    let mut reader = Cursor::new(data);
    let tex = read_tex(&mut reader)?;
    let video = tex.header.flags & 32 != 0 || tex.images.iter().any(|image| image.is_video_mp4);
    let duration = if video {
        let payload = tex
            .images
            .first()
            .and_then(|image| image.mipmaps.first())
            .context("video TEX payload")?
            .data;
        super::mp4::duration(payload)?.unwrap_or(0.)
    } else {
        0.
    };
    Ok(TextureInfo {
        size: [tex.header.image_width, tex.header.image_height],
        format: tex.header.format,
        frames: frames(
            &data[reader.position() as usize..],
            &tex.images,
            tex.header.flags,
        )?,
        video,
        duration,
    })
}
#[cfg(test)]
pub(super) fn load(data: &[u8]) -> Result<Texture> {
    load_compressed(data, |_| false)
}
pub(super) fn load_compressed(data: &[u8], supported: impl Fn(u32) -> bool) -> Result<Texture> {
    let mut reader = Cursor::new(data);
    let tex = read_tex(&mut reader)?;
    ensure!(
        !tex.images.is_empty() && tex.images.len() <= 256,
        "invalid TEX image count"
    );
    if tex.header.flags & 32 != 0 || tex.images.iter().any(|i| i.is_video_mp4) {
        ensure!(tex.images.len() == 1, "video TEX must contain one image");
        let mip = tex
            .images
            .into_iter()
            .next()
            .unwrap()
            .mipmaps
            .into_iter()
            .next()
            .context("video TEX has no payload")?;
        ensure!(
            !mip.is_lz4_compressed && mip.data.len() >= 12 && &mip.data[4..8] == b"ftyp",
            "invalid MP4 TEX payload"
        );
        let size = [tex.header.image_width, tex.header.image_height];
        ensure!(
            size.iter().all(|n| *n > 0 && *n <= 32768)
                && size[0] as u64 * size[1] as u64 * 4 <= LIMIT as u64,
            "video TEX size exceeds limit"
        );
        return Ok(Texture {
            compressed: None,
            mipmaps: Vec::new(),
            flags: tex.header.flags,
            video: Some(mip.data.to_vec()),
            width: size[0],
            height: size[1],
            content: size,
            rgba: Vec::new(),
            frames: Vec::new(),
        });
    }
    let bytes = tex.images.iter().try_fold(0u64, |bytes, image| {
        image.mipmaps.iter().try_fold(bytes, |bytes, mip| {
            bytes
                .checked_add(mip.width as u64 * mip.height as u64 * 4)
                .context("TEX image budget overflow")
        })
    })?;
    ensure!(bytes <= LIMIT as u64, "TEX decoded images exceed limit");
    let mut frames = frames(
        &data[reader.position() as usize..],
        &tex.images,
        tex.header.flags,
    )?;
    ensure!(
        tex.header.flags & 4 == 0 || !frames.is_empty(),
        "animated TEX has no valid frame table"
    );
    // A single image can upload validated BC blocks directly. Multi-image atlases
    // still need CPU pixels for packing, and unsupported GPUs retain RGBA fallback.
    let compressed = tex.images.len() == 1 && supported(tex.header.format);
    let mut images = tex
        .images
        .into_iter()
        .map(|image| decode(&tex.header, image, compressed))
        .collect::<Result<Vec<_>>>()?;
    let levels = if images.len() > 1 {
        images
            .iter()
            .map(|image| image.mipmaps.len())
            .min()
            .unwrap()
    } else {
        0
    };
    // Each image starts on the same row boundary in every retained mip level.
    let alignment = 1u32 << levels;
    let width = images.iter().map(|i| i.width).max().unwrap();
    let height = images.iter().try_fold(0u32, |n, i| {
        n.checked_add(i.height.div_ceil(alignment) * alignment)
            .context("TEX atlas height overflow")
    })?;
    let atlas_bytes: u64 = (0..=levels)
        .map(|level| (width >> level).max(1) as u64 * (height >> level).max(1) as u64 * 4)
        .sum();
    ensure!(
        height <= 32768 && atlas_bytes <= LIMIT as u64,
        "animated TEX atlas exceeds limit"
    );
    let mut offsets = Vec::new();
    let mut y = 0u32;
    for image in &images {
        offsets.push(y);
        y += image.height.div_ceil(alignment) * alignment;
    }
    for frame in &mut frames {
        frame.origin = [
            frame.origin[0] / width as f32,
            (frame.origin[1] + offsets[frame.image] as f32) / height as f32,
        ];
        frame.u = [frame.u[0] / width as f32, frame.u[1] / height as f32];
        frame.v = [frame.v[0] / width as f32, frame.v[1] / height as f32];
        // Preserve the authored frame aspect before atlas normalization.
        frame.ratio = frame.pixel_size[1] / frame.pixel_size[0];
    }
    if images.len() == 1 {
        let mut image = images.pop().unwrap();
        image.frames = frames;
        return Ok(image);
    }
    let rgba = atlas_level(&images, &offsets, [width, height], 0, levels > 0).rgba;
    let mipmaps = (1..=levels)
        .map(|level| {
            atlas_level(
                &images,
                &offsets,
                [(width >> level).max(1), (height >> level).max(1)],
                level,
                true,
            )
        })
        .collect();
    Ok(Texture {
        compressed: None,
        mipmaps,
        flags: tex.header.flags,
        video: None,
        width,
        height,
        content: [tex.header.image_width, tex.header.image_height],
        rgba,
        frames,
    })
}
fn atlas_level(
    images: &[Texture],
    offsets: &[u32],
    size: [u32; 2],
    level: usize,
    padded: bool,
) -> TextureMip {
    let mut rgba = vec![0; size[0] as usize * size[1] as usize * 4];
    for (index, image) in images.iter().enumerate() {
        let (source_size, pixels) = if level == 0 {
            ([image.width, image.height], &image.rgba)
        } else {
            let mip = &image.mipmaps[level - 1];
            (mip.size, &mip.rgba)
        };
        let offset = offsets[index] >> level;
        let rows = if padded {
            offsets.get(index + 1).map_or(size[1], |y| y >> level) - offset
        } else {
            source_size[1]
        };
        for row in 0..rows {
            let destination = ((offset + row) * size[0]) as usize * 4;
            let source = (row.min(source_size[1] - 1) * source_size[0]) as usize * 4;
            let length = source_size[0] as usize * 4;
            rgba[destination..destination + length]
                .copy_from_slice(&pixels[source..source + length]);
            if padded {
                let edge = &pixels[source + length - 4..source + length];
                for pixel in rgba[destination + length..destination + size[0] as usize * 4]
                    .as_chunks_mut::<4>()
                    .0
                {
                    pixel.copy_from_slice(edge);
                }
            }
        }
    }
    TextureMip {
        size,
        compressed: None,
        rgba,
    }
}
fn decode(header: &TexHeader, image: TexImage<'_>, compressed: bool) -> Result<Texture> {
    ensure!(image.mipmaps.len() <= 16, "TEX mip chain exceeds 16 levels");
    let mut levels = image.mipmaps.into_iter();
    let first = levels.next().context("TEX has no mipmap")?;
    let mut result = decode_level(header, image.image_format, first, compressed)?;
    let mut previous = [result.width, result.height];
    for mip in levels {
        let expected = previous.map(|size| (size / 2).max(1));
        ensure!(
            previous != [1, 1] && [mip.width, mip.height] == expected,
            "invalid TEX mip dimensions: {}x{}, expected {}x{}",
            mip.width,
            mip.height,
            expected[0],
            expected[1]
        );
        let mut header = header.clone();
        header.image_width = header.image_width.min(mip.width);
        header.image_height = header.image_height.min(mip.height);
        let level = decode_level(&header, image.image_format, mip, compressed)?;
        result.mipmaps.push(TextureMip {
            size: expected,
            compressed: level.compressed.map(|(_, bytes)| bytes),
            rgba: level.rgba,
        });
        previous = expected;
    }
    Ok(result)
}
fn decode_level(
    header: &TexHeader,
    image_format: i32,
    mip: TexMipmap<'_>,
    compressed_only: bool,
) -> Result<Texture> {
    let count = (mip.width as usize)
        .checked_mul(mip.height as usize)
        .context("texture size overflow")?;
    ensure!(
        count > 0 && count <= LIMIT / 4 && mip.width <= 32768 && mip.height <= 32768,
        "texture exceeds limit"
    );
    let mut data = if mip.is_lz4_compressed {
        ensure!(
            mip.decompressed_bytes_count as usize <= LIMIT,
            "TEX decompression exceeds limit"
        );
        Cow::Owned(lz4_flex::block::decompress(
            mip.data,
            mip.decompressed_bytes_count as usize,
        )?)
    } else {
        Cow::Borrowed(mip.data)
    };
    let compressed = if image_format < 0 && matches!(header.format, 4 | 6 | 7) {
        let expected = mip.width.div_ceil(4) as usize
            * mip.height.div_ceil(4) as usize
            * if header.format == 7 { 8 } else { 16 };
        ensure!(data.len() == expected, "invalid BC texture block length");
        Some((
            header.format,
            if compressed_only {
                std::mem::take(&mut data).into_owned()
            } else {
                data.to_vec()
            },
        ))
    } else {
        None
    };
    let rgba = if compressed_only && compressed.is_some() {
        Vec::new()
    } else if image_format >= 0 {
        let mut reader =
            image::ImageReader::new(Cursor::new(data.as_ref())).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(32768);
        limits.max_image_height = Some(32768);
        limits.max_alloc = Some(LIMIT as u64);
        reader.limits(limits);
        let decoded = reader.decode()?.into_rgba8();
        ensure!(
            decoded.width() == mip.width && decoded.height() == mip.height,
            "embedded image dimensions differ from TEX"
        );
        decoded.into_raw()
    } else {
        match header.format {
            0 => data.into_owned(),
            4 | 6 | 7 => {
                let mut pixels = vec![0u32; count];
                let decode = match header.format {
                    4 => texture2ddecoder::decode_bc3,
                    6 => texture2ddecoder::decode_bc2,
                    _ => texture2ddecoder::decode_bc1,
                };
                decode(
                    data.as_ref(),
                    mip.width as usize,
                    mip.height as usize,
                    &mut pixels,
                )
                .map_err(anyhow::Error::msg)?;
                pixels
                    .into_iter()
                    .flat_map(|pixel| {
                        [
                            (pixel >> 16) as u8,
                            (pixel >> 8) as u8,
                            pixel as u8,
                            (pixel >> 24) as u8,
                        ]
                    })
                    .collect()
            }
            8 => {
                ensure!(data.len() == count * 2, "invalid RG88 data");
                data.as_chunks::<2>()
                    .0
                    .iter()
                    .flat_map(|v| [v[0], v[1], 0, 255])
                    .collect()
            }
            9 => {
                ensure!(data.len() == count, "invalid R8 data");
                data.iter().flat_map(|&v| [v, v, v, 255]).collect()
            }
            other => anyhow::bail!("unsupported TEX format {other}"),
        }
    };
    ensure!(
        (compressed_only && compressed.is_some()) || rgba.len() == count * 4,
        "invalid RGBA data length"
    );
    ensure!(
        header.image_width > 0
            && header.image_width <= mip.width
            && header.image_height > 0
            && header.image_height <= mip.height,
        "invalid TEX content dimensions"
    );
    Ok(Texture {
        compressed,
        mipmaps: Vec::new(),
        flags: header.flags,
        video: None,
        frames: Vec::new(),
        width: mip.width,
        height: mip.height,
        content: [header.image_width, header.image_height],
        rgba,
    })
}
fn frames(data: &[u8], images: &[TexImage<'_>], flags: u32) -> Result<Vec<SpriteFrame>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(data.len() >= 13, "truncated TEX frame header");
    let magic = &data[..9];
    ensure!(
        [b"TEXS0001\0".as_slice(), b"TEXS0002\0", b"TEXS0003\0"].contains(&magic),
        "invalid TEX frame version"
    );
    let count = u32::from_le_bytes(data[9..13].try_into()?) as usize;
    ensure!(count > 0 && count <= 4096, "TEX frame count exceeds limit");
    let begin = if magic == b"TEXS0003\0" { 21 } else { 13 };
    ensure!(
        data.len() >= begin + count * 32,
        "truncated TEX frame table"
    );
    let mut out = Vec::with_capacity(count);
    for bytes in data[begin..begin + count * 32].as_chunks::<32>().0 {
        let integer = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let float = |offset| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let number = |offset| {
            if magic == b"TEXS0001\0" {
                i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as f32
            } else {
                float(offset)
            }
        };
        let image = integer(0) as usize;
        let mip = images
            .get(image)
            .and_then(|i| i.mipmaps.first())
            .context("TEX frame image outside table")?;
        let origin = [number(8), number(12)];
        let u = [number(16), number(20)];
        let v = [number(24), number(28)];
        let duration = float(4);
        let pixel_size = [u[0].hypot(u[1]), v[0].hypot(v[1])];
        ensure!(
            origin.iter().chain(&u).chain(&v).all(|v| v.is_finite())
                && duration.is_finite()
                && (0.0..=3600.0).contains(&duration)
                && pixel_size.iter().all(|v| *v > 0.0 && *v <= 32768.0),
            "invalid TEX frame values"
        );
        for (s, t) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let x = origin[0] + s * u[0] + t * v[0];
            let y = origin[1] + s * u[1] + t * v[1];
            ensure!(
                flags & 2 == 0
                    || (x >= -1.0
                        && x <= mip.width as f32 + 1.0
                        && y >= -1.0
                        && y <= mip.height as f32 + 1.0),
                "TEX frame escapes atlas"
            );
        }
        out.push(SpriteFrame {
            sequence: 0,
            image,
            duration,
            origin,
            u,
            v,
            pixel_size,
            ratio: pixel_size[1] / pixel_size[0],
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_upload_skips_rgba_and_keeps_atlas_fallback_and_validation() {
        fn blocks(kind: u32, images: u32) -> Vec<u8> {
            let mut data = b"TEXV0005\0TEXI0001\0".to_vec();
            for word in [kind, 0, 4, 4, 4, 4, 0] {
                data.extend(word.to_le_bytes());
            }
            data.extend(b"TEXB0001\0");
            data.extend(images.to_le_bytes());
            let length = if kind == 7 { 8u32 } else { 16 };
            for _ in 0..images {
                data.extend(2u32.to_le_bytes());
                for size in [4u32, 2] {
                    for word in [size, size, length] {
                        data.extend(word.to_le_bytes());
                    }
                    data.extend(vec![0; length as usize]);
                }
            }
            data
        }
        for kind in [4, 6, 7] {
            let data = blocks(kind, 1);
            let cpu = load(&data).unwrap();
            let gpu = load_compressed(&data, |format| format == kind).unwrap();
            assert!(gpu.rgba.is_empty() && gpu.mipmaps[0].rgba.is_empty());
            assert_eq!(gpu.compressed, cpu.compressed);
            assert_eq!(gpu.mipmaps[0].compressed, cpu.mipmaps[0].compressed);
            assert_eq!(gpu.content, cpu.content);
            assert_eq!(load_compressed(&data, |_| false).unwrap().rgba, cpu.rgba);
            let multi = blocks(kind, 2);
            let atlas = load_compressed(&multi, |_| true).unwrap();
            assert!(atlas.compressed.is_none());
            assert_eq!(atlas.rgba, load(&multi).unwrap().rgba);
            let mut malformed = data;
            let length = if kind == 7 { 8usize } else { 16 };
            let offset = malformed.len() - length - 4;
            malformed[offset..offset + 4].copy_from_slice(&(length as u32 + 1).to_le_bytes());
            malformed.push(0);
            assert!(load_compressed(&malformed, |_| true).is_err());
        }
    }
    fn texture(images: usize) -> Vec<u8> {
        let mut data = b"TEXV0005\0TEXI0001\0".to_vec();
        for n in [0u32, 6, 2, 1, 1, 1, 0] {
            data.extend(n.to_le_bytes());
        }
        data.extend(b"TEXB0001\0");
        data.extend((images as u32).to_le_bytes());
        for i in 0..images {
            for n in [1u32, 2, 1, 8] {
                data.extend(n.to_le_bytes());
            }
            data.extend(if i == 0 {
                [255, 0, 0, 255, 0, 0, 255, 255]
            } else {
                [0, 255, 0, 255, 255, 255, 255, 255]
            });
        }
        data
    }
    fn frame(image: u32, origin: [f32; 2], u: [f32; 2], v: [f32; 2]) -> Vec<u8> {
        let mut data = image.to_le_bytes().to_vec();
        for n in [0.25, origin[0], origin[1], u[0], u[1], v[0], v[1]] {
            data.extend(n.to_le_bytes());
        }
        data
    }
    #[test]
    fn metadata_and_decode_reject_truncated_or_out_of_bounds_payloads() {
        let mut data = texture(2);
        data[22..26].copy_from_slice(&0u32.to_le_bytes());
        let info = metadata(&data).unwrap();
        let texture = load(&data).unwrap();
        assert_eq!(info.size, texture.content);
        assert!(!info.video && info.frames.is_empty());
        for end in 0..data.len() {
            assert!(metadata(&data[..end]).is_err(), "metadata prefix {end}");
            assert!(load(&data[..end]).is_err(), "decode prefix {end}");
        }
        for byte_count in [-1i32, 512 * 1024 * 1024 + 1, 1000] {
            let mut invalid = data.clone();
            // First mip payload length follows the two dimensions.
            invalid[71..75].copy_from_slice(&byte_count.to_le_bytes());
            assert!(metadata(&invalid).is_err());
            assert!(load(&invalid).is_err());
        }
    }
    #[test]
    fn atlas_affine_frames_and_multiple_image_animation() {
        let mut data = texture(1);
        data.extend(b"TEXS0003\0");
        for n in [2u32, 1, 1] {
            data.extend(n.to_le_bytes());
        }
        data.extend(frame(0, [0.0, 0.0], [1.0, 0.0], [0.0, 1.0]));
        data.extend(frame(0, [2.0, 0.0], [-1.0, 0.0], [0.0, 1.0]));
        let tex = load(&data).unwrap();
        assert_eq!(tex.frames.len(), 2);
        assert_eq!(tex.frames[1].origin, [1.0, 0.0]);
        assert_eq!(tex.frames[1].u, [-0.5, 0.0]);
        let mut invalid = data.clone();
        let offset = invalid.len() - 32 + 8;
        invalid[offset..offset + 4].copy_from_slice(&200.0_f32.to_le_bytes());
        assert!(load(&invalid).is_err());
        let mut multi = texture(2);
        multi.extend(b"TEXS0002\0");
        multi.extend(2u32.to_le_bytes());
        multi.extend(frame(0, [0.0, 0.0], [2.0, 0.0], [0.0, 1.0]));
        multi.extend(frame(1, [0.0, 0.0], [2.0, 0.0], [0.0, 1.0]));
        let tex = load(&multi).unwrap();
        assert_eq!([tex.width, tex.height], [2, 2]);
        assert_eq!(tex.frames[1].origin, [0.0, 0.5]);
        assert_eq!(tex.frames[1].v, [0.0, 0.5]);
        assert_eq!(&tex.rgba[8..12], &[0, 255, 0, 255]);
        assert!(load(&data[..data.len() - 1]).is_err());
    }
    #[test]
    fn non_power_of_two_animation_preserves_authored_mips_and_frame_boundaries() {
        let mut data = b"TEXV0005\0TEXI0001\0".to_vec();
        for word in [0u32, 6, 3, 3, 3, 3, 0] {
            data.extend(word.to_le_bytes());
        }
        data.extend(b"TEXB0001\0");
        data.extend(2u32.to_le_bytes());
        for (base, mip) in [
            ([255, 0, 0, 255], [0, 255, 0, 255]),
            ([0, 0, 255, 255], [255, 255, 0, 255]),
        ] {
            data.extend(2u32.to_le_bytes());
            for (size, color) in [(3u32, base), (1, mip)] {
                for word in [size, size, size * size * 4] {
                    data.extend(word.to_le_bytes());
                }
                data.extend(color.repeat(size as usize * size as usize));
            }
        }
        data.extend(b"TEXS0002\0");
        data.extend(2u32.to_le_bytes());
        for image in [0, 1] {
            data.extend(frame(image, [0., 0.], [3., 0.], [0., 3.]));
        }
        let tex = load(&data).unwrap();
        assert_eq!([tex.width, tex.height], [3, 8]);
        assert_eq!(tex.frames[1].origin, [0., 0.5]);
        assert_eq!(tex.frames[1].v, [0., 0.375]);
        assert_eq!(tex.mipmaps.len(), 1);
        assert_eq!(tex.mipmaps[0].size, [1, 4]);
        assert_eq!(
            tex.mipmaps[0].rgba,
            [
                0, 255, 0, 255, 0, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255
            ]
        );
        assert_eq!(&tex.rgba[36..48], &[255, 0, 0, 255].repeat(3));
    }
    #[test]
    fn integer_frames_preserve_signed_rotated_axes() {
        let mut data = texture(1);
        data.extend(b"TEXS0001\0");
        data.extend(1u32.to_le_bytes());
        data.extend(0u32.to_le_bytes());
        data.extend(0.25f32.to_le_bytes());
        for n in [2i32, 0, 0, 1, -2, 0] {
            data.extend(n.to_le_bytes());
        }
        let tex = load(&data).unwrap();
        assert_eq!(tex.frames[0].u, [0., 1.]);
        assert_eq!(tex.frames[0].v, [-1., 0.]);
        assert!(load(&texture(1)).is_err()); // GIF flag requires its frame table.
        data[22..26].copy_from_slice(&32u32.to_le_bytes());
        assert!(load(&data).err().unwrap().to_string().contains("MP4"));
    }
}

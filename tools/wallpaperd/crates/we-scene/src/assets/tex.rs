//! Derived from Yueosa/lianpkg@64b75310730e78a3aa8b1aed427e924121da1854.
use anyhow::{Result, anyhow};
use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Read, Seek};

/// 读取 TEX 文件结构
pub(super) fn read_tex<'a>(mut reader: &mut Cursor<&'a [u8]>) -> Result<TexFile<'a>> {
    let magic1 = read_n_string(&mut reader, 16)?;
    if magic1 != "TEXV0005" {
        return Err(anyhow!("Invalid data: Invalid Magic1: '{}'", magic1));
    }

    let magic2 = read_n_string(&mut reader, 16)?;
    if magic2 != "TEXI0001" {
        return Err(anyhow!("Invalid data: Invalid Magic2: '{}'", magic2));
    }

    let header = read_header(&mut reader)?;
    let images = read_image_container(reader)?;

    Ok(TexFile { header, images })
}

fn read_header<R: Read + Seek>(reader: &mut R) -> Result<TexHeader> {
    let format = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let flags = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let _texture_width = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let _texture_height = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let image_width = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let image_height = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let _unk_int0 = reader.read_u32::<LittleEndian>().map_err(read_err)?;

    Ok(TexHeader {
        format,
        flags,
        image_width,
        image_height,
    })
}

fn read_image_container<'a>(reader: &mut Cursor<&'a [u8]>) -> Result<Vec<TexImage<'a>>> {
    let magic = read_n_string(reader, 16)?;
    let image_count = reader.read_i32::<LittleEndian>().map_err(read_err)?;

    // 合理性检查
    if !(0..=1000).contains(&image_count) {
        return Err(anyhow!(
            "Invalid data: image_count {} out of valid range [0, 1000]",
            image_count
        ));
    }

    let mut image_format: i32 = -1; // Default to FIF_UNKNOWN
    let mut is_video_mp4 = false;
    let mut version = 0;

    if let Some(stripped) = magic.strip_prefix("TEXB")
        && let Ok(v) = stripped.parse::<i32>()
    {
        version = v;
    }

    match magic.as_str() {
        "TEXB0001" | "TEXB0002" => {}
        "TEXB0003" => {
            image_format = reader.read_i32::<LittleEndian>().map_err(read_err)?;
        }
        "TEXB0004" => {
            image_format = reader.read_i32::<LittleEndian>().map_err(read_err)?;
            is_video_mp4 = reader.read_i32::<LittleEndian>().map_err(read_err)? == 1;
        }
        _ => {
            return Err(anyhow!(
                "Invalid data: Unknown ImageContainer Magic: '{}'",
                magic
            ));
        }
    }

    let effective_version = if version == 4 && !is_video_mp4 {
        3
    } else {
        version
    };

    let mut images = Vec::new();
    for _ in 0..image_count {
        images.push(read_image(
            reader,
            effective_version,
            image_format,
            is_video_mp4,
        )?);
    }

    Ok(images)
}

fn read_image<'a>(
    reader: &mut Cursor<&'a [u8]>,
    version: i32,
    image_format: i32,
    is_video_mp4: bool,
) -> Result<TexImage<'a>> {
    let mipmap_count = reader.read_i32::<LittleEndian>().map_err(read_err)?;

    // 合理性检查
    if !(0..=100).contains(&mipmap_count) {
        return Err(anyhow!(
            "Invalid data: mipmap_count {} out of valid range [0, 100]",
            mipmap_count
        ));
    }

    let mut mipmaps = Vec::new();

    for _ in 0..mipmap_count {
        mipmaps.push(read_mipmap(reader, version)?);
    }

    Ok(TexImage {
        image_format,
        is_video_mp4,
        mipmaps,
    })
}

fn read_mipmap<'a>(reader: &mut Cursor<&'a [u8]>, version: i32) -> Result<TexMipmap<'a>> {
    if version == 4 {
        // V4 specific fields
        let _param1 = reader.read_i32::<LittleEndian>().map_err(read_err)?;
        let _param2 = reader.read_i32::<LittleEndian>().map_err(read_err)?;
        let _condition_json = read_n_string(reader, 0)?;
        let _param3 = reader.read_i32::<LittleEndian>().map_err(read_err)?;
    }

    let width = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    let height = reader.read_u32::<LittleEndian>().map_err(read_err)?;

    let mut is_lz4_compressed = false;
    let mut decompressed_bytes_count = 0;

    if version >= 2 {
        is_lz4_compressed = reader.read_i32::<LittleEndian>().map_err(read_err)? == 1;
        decompressed_bytes_count = reader.read_u32::<LittleEndian>().map_err(read_err)?;
    }

    let byte_count = reader.read_i32::<LittleEndian>().map_err(read_err)?;

    // 合理性检查
    if byte_count < 0 {
        return Err(anyhow!(
            "Invalid data: mipmap byte_count {} is negative",
            byte_count
        ));
    }
    // 防止恶意文件触发巨量分配（512 MB 上限）
    if byte_count as u64 > 512 * 1024 * 1024 {
        return Err(anyhow!(
            "Invalid data: mipmap byte_count {} exceeds 512 MB limit",
            byte_count
        ));
    }

    let position = reader.position();
    let end = reader.get_ref().len() as u64;
    if byte_count as u64 > end.saturating_sub(position) {
        return Err(anyhow!("Invalid data: mipmap exceeds remaining input"));
    }
    // The package snapshot owns these bytes. Metadata inspection needs no
    // payload copies; decoding materializes only the buffers it retains.
    let data = &reader.get_ref()[position as usize..position as usize + byte_count as usize];
    reader.set_position(position + byte_count as u64);

    Ok(TexMipmap {
        width,
        height,
        is_lz4_compressed,
        decompressed_bytes_count,
        data,
    })
}

/// 读取 null-terminated 字符串，逐字节读取直到遇到 `\0`。
///
/// `max_length == 0` 时使用 4096 字节上限（用于 V4 condition_json 等变长字段）。
fn read_n_string<R: Read>(reader: &mut R, max_length: usize) -> Result<String> {
    let max_length = if max_length == 0 { 4096 } else { max_length };
    let mut bytes = Vec::new();
    let mut c = [0u8; 1];

    loop {
        reader.read_exact(&mut c).map_err(read_err)?;
        if c[0] == 0 {
            break;
        }
        bytes.push(c[0]);
        if max_length > 0 && bytes.len() >= max_length {
            return Err(anyhow!("Invalid data: TEX string exceeds limit"));
        }
    }

    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// 为 TEX 读取错误补充格式上下文。
fn read_err(e: std::io::Error) -> anyhow::Error {
    anyhow!("Invalid data: TEX read error: {}", e)
}

/// TEX 文件完整结构（内部使用）
#[derive(Debug, Clone)]
pub(super) struct TexFile<'a> {
    pub header: TexHeader,
    pub images: Vec<TexImage<'a>>,
}

/// TEX 文件头
#[derive(Debug, Clone)]
pub(super) struct TexHeader {
    pub format: u32,
    pub flags: u32,
    pub image_width: u32,
    pub image_height: u32,
}

/// TEX 图像
#[derive(Debug, Clone)]
pub(super) struct TexImage<'a> {
    pub image_format: i32,
    pub is_video_mp4: bool,
    pub mipmaps: Vec<TexMipmap<'a>>,
}

/// TEX Mipmap
#[derive(Debug, Clone)]
pub(super) struct TexMipmap<'a> {
    pub width: u32,
    pub height: u32,
    pub is_lz4_compressed: bool,
    pub decompressed_bytes_count: u32,
    pub data: &'a [u8],
}

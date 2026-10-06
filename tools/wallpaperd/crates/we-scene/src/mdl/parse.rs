//! Checked, length-driven parsing; vertex layouts are determined by their declared flags.
use super::*;
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).context("MDL offset overflow")?;
        let bytes = self.data.get(self.pos..end).context("truncated MDL")?;
        self.pos = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn f32(&mut self) -> Result<f32> {
        let n = f32::from_le_bytes(self.take(4)?.try_into()?);
        ensure!(n.is_finite(), "non-finite MDL value");
        Ok(n)
    }
    fn array<const N: usize>(&mut self) -> Result<[f32; N]> {
        let mut a = [0.0; N];
        for x in &mut a {
            *x = self.f32()?;
        }
        Ok(a)
    }
    fn string(&mut self) -> Result<String> {
        let bytes = self.data.get(self.pos..).context("MDL string offset")?;
        let len = bytes
            .iter()
            .take(65537)
            .position(|&b| b == 0)
            .context("unterminated/oversized MDL string")?;
        let text = std::str::from_utf8(self.take(len)?)?.to_owned();
        self.take(1)?;
        Ok(text)
    }
    fn matrix(&mut self) -> Result<Mat4> {
        let mut m = Mat4::from_cols_array(&self.array::<16>()?);
        // 2D puppet files can omit the unused Z basis; retain their invertible XY transform.
        if m.determinant().abs() < 1e-12
            && m.z_axis.truncate() == Vec3::ZERO
            && m.x_axis.z == 0.0
            && m.y_axis.z == 0.0
            && m.w_axis.w == 1.0
        {
            m.z_axis = Vec4::Z;
        }
        ensure!(
            m.determinant().abs() > 1e-12 && m.inverse().is_finite(),
            "singular MDL transform"
        );
        Ok(m)
    }
}
struct Layout {
    stride: usize,
    normal: Option<usize>,
    tangent: Option<usize>,
    bones: Option<usize>,
    weights: Option<usize>,
    uv: Option<usize>,
    uv2: Option<usize>,
}
fn layout(flag: u32) -> Layout {
    let mut offset = 12;
    let mut field = |bit: u32, size: usize| {
        if flag & bit != 0 {
            let start = offset;
            offset += size;
            Some(start)
        } else {
            None
        }
    };
    let normal = field(2, 12);
    let tangent = field(4, 16);
    field(0x10000, 4);
    let bones = field(0x800000, 16);
    let weights = field(0x1000000, 16);
    let uv = field(8 | 32, 8);
    let uv2 = field(32, 8);
    Layout {
        stride: offset,
        normal,
        tangent,
        bones,
        weights,
        uv,
        uv2,
    }
}
pub(crate) fn parse(data: &[u8]) -> Result<Model> {
    ensure!(data.len() <= 256 * 1024 * 1024, "MDL exceeds 256 MiB");
    let mut r = Reader { data, pos: 0 };
    let magic = r.take(8)?;
    ensure!(magic.starts_with(b"MDLV"), "invalid MDL header");
    let version = std::str::from_utf8(&magic[4..])?.parse::<u32>()?;
    ensure!(
        (4..=23).contains(&version),
        "unsupported MDL version {version}"
    );
    ensure!(r.u8()? == 0, "invalid MDL header terminator");
    let flag = r.u32()?;
    let skins = r.u32()? as usize;
    let mesh_count = r.u32()? as usize;
    ensure!(
        (1..=32).contains(&skins) && (1..=512).contains(&mesh_count),
        "invalid MDL mesh/skin counts"
    );
    let mut meshes = Vec::new();
    let mut total_vertices = 0;
    let mut total_indices = 0;
    for _ in 0..mesh_count {
        let materials = (0..skins).map(|_| r.string()).collect::<Result<Vec<_>>>()?;
        for material in &materials {
            crate::assets::normalize(material)?;
        }
        let kind = r.u32()?;
        ensure!(kind <= 2, "invalid MDL geometry type");
        if kind == 2 {
            r.u32()?;
        }
        if version >= 17 {
            r.array::<6>()?;
        }
        let flags = if version > 14 { r.u32()? } else { flag };
        let lay = layout(flags);
        let vertex_bytes = r.u32()? as usize;
        ensure!(
            vertex_bytes.is_multiple_of(lay.stride),
            "unaligned MDL vertex data"
        );
        let count = vertex_bytes / lay.stride;
        total_vertices += count;
        ensure!(
            count > 0 && total_vertices <= 2_000_000,
            "MDL vertex budget exceeded"
        );
        let bytes = r.take(vertex_bytes)?;
        let mut vertices = Vec::with_capacity(count);
        for i in 0..count {
            let mut v = Reader {
                data: &bytes[i * lay.stride..(i + 1) * lay.stride],
                pos: 0,
            };
            let position = Vec3::from_array(v.array()?);
            let mut read = |offset: Option<usize>, default: &[f32]| -> Result<Vec<f32>> {
                if let Some(offset) = offset {
                    v.pos = offset;
                    (0..default.len()).map(|_| v.f32()).collect()
                } else {
                    Ok(default.to_vec())
                }
            };
            let normal = Vec3::from_slice(&read(lay.normal, &[0.0, 0.0, 1.0])?);
            let tangent = Vec4::from_slice(&read(lay.tangent, &[1.0, 0.0, 0.0, 1.0])?);
            let uv = Vec2::from_slice(&read(lay.uv, &[0.0; 2])?);
            let uv2 = Vec2::from_slice(&read(lay.uv2, &uv.to_array())?);
            let weights = Vec4::from_slice(&read(lay.weights, &[0.0; 4])?);
            ensure!(
                weights.min_element() >= 0.0 && weights.element_sum() <= 1.05,
                "invalid MDL skin weights"
            );
            let mut bones = [0; 4];
            if let Some(offset) = lay.bones {
                v.pos = offset;
                for b in &mut bones {
                    *b = v.u32()? as usize;
                }
            }
            vertices.push(Vertex {
                position,
                normal,
                tangent,
                uv,
                uv2,
                bones,
                weights,
            });
        }
        let index_bytes = r.u32()? as usize;
        let wide = version >= 23 && count > 65535;
        let width = if wide { 4 } else { 2 };
        ensure!(
            index_bytes.is_multiple_of(width * 3),
            "invalid MDL triangles"
        );
        total_indices += index_bytes / width;
        ensure!(total_indices <= 6_000_000, "MDL index budget exceeded");
        let mut indices = Vec::with_capacity(index_bytes / width);
        for _ in 0..index_bytes / width {
            let index = if wide { r.u32()? } else { r.u16()? as u32 };
            ensure!((index as usize) < count, "MDL index outside vertex buffer");
            indices.push(index);
        }
        let mut parts = Vec::new();
        if version >= 21 {
            match r.u8()? {
                0 => {}
                1 => {
                    if r.u8()? != 0 {
                        r.take(3)?;
                        let bytes = r.u32()? as usize;
                        ensure!(bytes.is_multiple_of(12), "invalid MDL auxiliary vertices");
                        r.take(bytes)?;
                    }
                }
                _ => anyhow::bail!("invalid MDL vertex section"),
            }
            if r.u8()? != 0 {
                let bytes = r.u32()? as usize;
                ensure!(
                    bytes.is_multiple_of(16) && bytes / 16 <= 4096,
                    "invalid MDL parts"
                );
                for _ in 0..bytes / 16 {
                    let id = r.u32()?;
                    let offset = r.u32()? as i32;
                    let start = r.u32()? as usize;
                    let size = r.u32()? as usize;
                    ensure!(
                        start
                            .checked_add(size)
                            .is_some_and(|end| end <= indices.len()),
                        "MDL part outside mesh"
                    );
                    parts.push(Part {
                        id,
                        offset,
                        start,
                        count: size,
                    });
                }
            }
            if version > 21 {
                let masks = r.u32()? as usize;
                ensure!(masks <= 4096, "too many MDL material masks");
                for _ in 0..masks {
                    r.take(8)?;
                    let material = r.string()?;
                    if !material.is_empty() {
                        crate::assets::normalize(&material)?;
                    }
                    r.take(4)?;
                    for _ in 0..2 {
                        let count = r.u32()? as usize;
                        ensure!(count <= total_vertices, "invalid MDL mask indices");
                        r.take(count * 4)?;
                    }
                }
            }
        }
        meshes.push(Geometry {
            materials,
            vertices,
            indices,
            parts,
        });
    }
    let mut sections = HashMap::new();
    for offset in r.pos..data.len().saturating_sub(8) {
        if &data[offset..offset + 2] != b"MD" {
            continue;
        }
        for tag in [b"MDLS", b"MDLA", b"MDLE", b"MDAT"] {
            if &data[offset..offset + 4] == tag
                && data[offset + 4..offset + 8].iter().all(u8::is_ascii_digit)
                && data[offset + 8] == 0
            {
                ensure!(
                    sections.insert(*tag, offset).is_none(),
                    "duplicate MDL section"
                );
            }
        }
    }
    let bones = skeleton(data, sections.get(b"MDLS").copied())?;
    for mesh in &meshes {
        for v in &mesh.vertices {
            for i in 0..4 {
                ensure!(
                    v.weights[i] == 0.0 || v.bones[i] < bones.len(),
                    "MDL vertex refers to missing bone"
                );
            }
        }
    }
    let clips = animations(data, sections.get(b"MDLA").copied(), bones.len())?;
    let rest = if let Some(offset) = sections.get(b"MDLE") {
        let mut r = Reader {
            data,
            pos: offset + 9,
        };
        r.u32()?;
        let bytes = r.u32()? as usize;
        ensure!(bytes == bones.len() * 64, "invalid MDL rest pose length");
        Some(
            (0..bones.len())
                .map(|_| r.matrix())
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        None
    };
    let attachments = if let Some(offset) = sections.get(b"MDAT") {
        attachments(data, *offset, bones.len())?
    } else {
        Vec::new()
    };
    Ok(Model {
        meshes,
        bones,
        clips,
        rest,
        attachments,
    })
}
fn skeleton(data: &[u8], offset: Option<usize>) -> Result<Vec<Bone>> {
    let Some(offset) = offset else {
        return Ok(Vec::new());
    };
    let mut r = Reader {
        data,
        pos: offset + 9,
    };
    let end = (r.u32()? & 0xFFFFFF) as usize;
    let count = r.u32()? as usize;
    ensure!(count <= 1024, "MDL skeleton exceeds 1024 bones");
    let mut bones = Vec::with_capacity(count);
    for index in 0..count {
        let name = r.string()?;
        r.u32()?;
        let parent = r.u32()?;
        ensure!(
            parent == u32::MAX || ((parent as usize) < count && parent as usize != index),
            "invalid MDL bone parent"
        );
        ensure!(r.u32()? == 64, "invalid MDL bone matrix size");
        let local = r.matrix()?;
        let metadata = r.string()?;
        let simulation = if metadata.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&metadata)?
        };
        bones.push(Bone {
            name,
            parent: if parent == u32::MAX {
                None
            } else {
                Some(parent as usize)
            },
            local,
            simulation,
        });
    }
    ensure!(
        end == 0 || (end <= data.len() && r.pos <= end),
        "MDL skeleton exceeds its section"
    );
    for index in 0..count {
        let mut current = bones[index].parent;
        let mut depth = 0;
        while let Some(parent) = current {
            depth += 1;
            ensure!(depth < count, "MDL skeleton cycle");
            current = bones[parent].parent;
        }
    }
    Ok(bones)
}
fn attachments(data: &[u8], offset: usize, bones: usize) -> Result<Vec<Attachment>> {
    let mut r = Reader {
        data,
        pos: offset + 9,
    };
    r.u32()?;
    let count = r.u16()? as usize;
    ensure!(count <= 4096, "too many MDL attachments");
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let bone = r.u16()? as usize;
        let name = r.string()?;
        ensure!(bone < bones, "MDL attachment refers to missing bone");
        let matrix = r.matrix()?;
        out.push(Attachment { name, bone, matrix });
    }
    Ok(out)
}
fn clip_header(
    r: &mut Reader<'_>,
    bones: usize,
) -> Result<(u32, String, String, f32, usize, usize)> {
    let id = r.u32()?;
    r.u32()?;
    let name = r.string()?;
    let mode = r.string()?;
    let fps = r.f32()?;
    let frames = r.u32()? as usize;
    r.u32()?;
    let tracks = r.u32()? as usize;
    ensure!(
        !name.is_empty()
            && name.len() <= 1024
            && ["loop", "once", "single", "mirror", "pingpong", "runonce"].contains(&mode.as_str())
            && fps > 0.0
            && fps <= 1000.0
            && frames > 0
            && frames <= 200_000
            && tracks <= bones,
        "invalid MDL animation header"
    );
    let mode = if matches!(mode.as_str(), "once" | "runonce") {
        "single".into()
    } else {
        mode
    };
    Ok((id, name, mode, fps, frames, tracks))
}
fn animations(data: &[u8], offset: Option<usize>, bones: usize) -> Result<Vec<Clip>> {
    let Some(offset) = offset else {
        return Ok(Vec::new());
    };
    let mut r = Reader {
        data,
        pos: offset + 9,
    };
    let end = r.u32()? as usize;
    let count = r.u32()? as usize;
    ensure!(
        count <= 256 && end <= data.len(),
        "invalid MDL animation section"
    );
    let mut out = Vec::with_capacity(count);
    let mut total = 0;
    for index in 0..count {
        let (id, name, mode, fps, frames, track_count) = clip_header(&mut r, bones)?;
        let mut tracks = Vec::with_capacity(track_count);
        for _ in 0..track_count {
            r.u32()?;
            let bytes = r.u32()? as usize;
            ensure!(bytes.is_multiple_of(36), "invalid MDL animation track");
            let key_count = bytes / 36;
            total += key_count;
            ensure!(
                key_count <= 200_001 && total <= 2_000_000,
                "MDL animation budget exceeded"
            );
            let mut keys = Vec::with_capacity(key_count);
            for _ in 0..key_count {
                let translation = Vec3::from_array(r.array()?);
                let angles = Vec3::from_array(r.array()?);
                let scale = Vec3::from_array(r.array()?);
                keys.push(Key {
                    translation,
                    angles,
                    scale,
                });
            }
            tracks.push(keys);
        }
        let mut events = Vec::new();
        let tail = r.pos;
        let limit = end.min(data.len());
        for pos in tail..(tail + 8192).min(limit) {
            if data[pos] != b'{' {
                continue;
            }
            let mut event = Reader { data, pos };
            if let Ok(text) = event.string()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                && let (Some(frame), Some(name)) = (value["frame"].as_f64(), value["name"].as_str())
            {
                ensure!(events.len() < 4096, "too many MDL animation events");
                events.push((frame as f32, name.to_owned()));
            }
        }
        out.push(Clip {
            id,
            name,
            mode,
            fps,
            frames,
            tracks,
            events,
        });
        if index + 1 < count {
            let mut next = None;
            for pos in tail..(tail + 65536).min(limit) {
                let mut candidate = Reader { data, pos };
                if clip_header(&mut candidate, bones).is_ok() {
                    next = Some(pos);
                    break;
                }
            }
            r.pos = next.context("MDL animation header is missing")?;
        }
    }
    ensure!(r.pos <= end, "MDL animation outside section");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires extracted real WE MDL assets"]
    fn real_assets_parse_without_partial_skeletons() {
        let root = std::env::var("WE_MDL_ASSETS").expect("WE_MDL_ASSETS inventory directory");
        let inventory: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::path::Path::new(&root).join("inventory.json")).unwrap(),
        )
        .unwrap();
        let mut failures = Vec::new();
        for entry in inventory.as_array().unwrap() {
            let path = entry["file"].as_str().unwrap();
            let data = std::fs::read(path).unwrap();
            match parse(&data) {
                Ok(model) => eprintln!(
                    "{path}: {} meshes / {} vertices / {} bones / {} clips / {} attachments",
                    model.meshes.len(),
                    model.meshes.iter().map(|m| m.vertices.len()).sum::<usize>(),
                    model.bones.len(),
                    model.clips.len(),
                    model.attachments.len()
                ),
                Err(error) => failures.push(format!("{path}: {error:#}")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

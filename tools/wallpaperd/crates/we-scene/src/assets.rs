mod mp4;
mod pkg;
mod sheet;
mod tex;
mod texture;
use self::pkg::parse_pkg;
use anyhow::{Context, Result, ensure};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufReader, Read},
    ops::Range,
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

const LIMIT: usize = 512 * 1024 * 1024;
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum AssetKey {
    Package(String),
    File(PathBuf),
}
#[derive(Clone)]
pub(crate) struct Assets {
    data: std::rc::Rc<PackageData>,
    entries: std::rc::Rc<HashMap<String, Range<usize>>>,
    roots: Vec<PathBuf>,
    texture_info: std::rc::Rc<std::cell::RefCell<HashMap<AssetKey, std::rc::Rc<TextureInfo>>>>,
}
enum PackageData {
    Json(Vec<u8>),
    Indexed {
        file: File,
        version: (u64, i64, i64),
    },
}
impl PackageData {
    fn version(file: &File) -> Result<(u64, i64, i64)> {
        let metadata = file.metadata()?;
        Ok((metadata.len(), metadata.mtime(), metadata.mtime_nsec()))
    }
    fn read(&self, range: Range<usize>) -> Result<Cow<'_, [u8]>> {
        match self {
            Self::Json(data) => Ok(Cow::Borrowed(&data[range])),
            Self::Indexed { file, version } => {
                // The open inode survives atomic Workshop replacements. Refuse
                // in-place changes rather than applying stale entry offsets.
                ensure!(
                    Self::version(file)? == *version,
                    "scene package changed while loaded"
                );
                let mut bytes = vec![0; range.len()];
                file.read_exact_at(&mut bytes, range.start as u64)?;
                ensure!(
                    Self::version(file)? == *version,
                    "scene package changed while reading"
                );
                Ok(Cow::Owned(bytes))
            }
        }
    }
}
impl Assets {
    pub fn project(&self) -> &Path {
        &self.roots[0]
    }
    pub fn open(project: &Path, package: &Path, common: Option<&Path>) -> Result<Self> {
        let project = project.canonicalize()?;
        let package = package.canonicalize()?;
        ensure!(package.starts_with(&project), "package escapes project");
        let mut entries = HashMap::new();
        let data = if package
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            let data = bounded_read_limit(&package, 8 * 1024 * 1024)?;
            let _: serde_json::Value =
                serde_json::from_slice(data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data))?;
            entries.insert("scene.json".into(), 0..data.len());
            PackageData::Json(data)
        } else {
            let file = bounded_open(&package, LIMIT)?;
            let version = PackageData::version(&file)?;
            ensure!(version.0 <= LIMIT as u64, "scene package exceeds 512 MiB");
            let info = parse_pkg(BufReader::new(&file), version.0 as usize)?;
            ensure!(
                info.version.starts_with("PKGV"),
                "unsupported PKG version {}",
                info.version
            );
            for entry in info.entries {
                let name = normalize(&entry.name)?;
                let start = info
                    .data_start
                    .checked_add(entry.offset as usize)
                    .context("PKG offset overflow")?;
                let end = start
                    .checked_add(entry.size as usize)
                    .context("PKG size overflow")?;
                ensure!(end <= version.0 as usize, "PKG entry outside package");
                ensure!(
                    entries.insert(name, start..end).is_none(),
                    "duplicate PKG entry"
                );
            }
            ensure!(
                PackageData::version(&file)? == version,
                "scene package changed while indexing"
            );
            PackageData::Indexed { file, version }
        };
        let mut roots = vec![project];
        if let Some(common) = common {
            roots.push(common.canonicalize()?);
        }
        Ok(Self {
            data: std::rc::Rc::new(data),
            entries: std::rc::Rc::new(entries),
            roots,
            texture_info: Default::default(),
        })
    }
    /// Common effects ship their materials/shaders under the effect directory.
    /// Keep that lookup scope local to this effect, sharing the indexed package.
    pub fn effect(&self, name: &str) -> Result<Self> {
        let name = normalize(name)?;
        let parent = Path::new(&name).parent().context("effect directory")?;
        let mut roots = self.roots.clone();
        for root in &self.roots {
            if let Ok(directory) = root.join(parent).canonicalize() {
                ensure!(directory.starts_with(root), "effect directory escapes root");
                if directory.is_dir() && !roots.contains(&directory) {
                    roots.push(directory);
                }
            }
        }
        Ok(Self {
            data: self.data.clone(),
            entries: self.entries.clone(),
            roots,
            texture_info: self.texture_info.clone(),
        })
    }
    fn locate(&self, name: &str) -> Result<AssetKey> {
        self.locate_optional(name)?
            .with_context(|| format!("asset not found: {name}"))
    }
    fn locate_optional(&self, name: &str) -> Result<Option<AssetKey>> {
        let name = normalize(name)?;
        if self.entries.contains_key(&name) {
            return Ok(Some(AssetKey::Package(name)));
        }
        for root in &self.roots {
            if let Ok(path) = root.join(&name).canonicalize() {
                ensure!(path.starts_with(root), "asset escapes root: {name}");
                if path.is_file() {
                    return Ok(Some(AssetKey::File(path)));
                }
            }
        }
        Ok(None)
    }
    pub fn validate(&self, name: &str) -> Result<()> {
        self.locate(name).map(|_| ())
    }
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        Ok(self.bytes(name)?.into_owned())
    }
    fn bytes(&self, name: &str) -> Result<Cow<'_, [u8]>> {
        self.bytes_limit(name, LIMIT)
    }
    fn bytes_limit(&self, name: &str, limit: usize) -> Result<Cow<'_, [u8]>> {
        match self.locate(name)? {
            AssetKey::Package(name) => {
                let range = self.entries[&name].clone();
                ensure!(
                    range.len() <= limit,
                    "asset exceeds {} MiB",
                    limit / (1024 * 1024)
                );
                self.data.read(range)
            }
            AssetKey::File(path) => bounded_read_limit(&path, limit).map(Cow::Owned),
        }
    }
    pub fn optional_read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.locate_optional(name)?
            .map(|key| match key {
                AssetKey::Package(name) => self
                    .data
                    .read(self.entries[&name].clone())
                    .map(Cow::into_owned),
                AssetKey::File(path) => bounded_read(&path),
            })
            .transpose()
    }
    pub fn texture_key(&self, name: &str) -> Result<AssetKey> {
        self.locate(&texture_name(name)?)
    }
    pub fn json(&self, name: &str) -> Result<serde_json::Value> {
        let data = self.bytes_limit(name, 8 * 1024 * 1024)?;
        let value: serde_json::Value =
            serde_json::from_slice(data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data))
                .with_context(|| format!("reading {name}"))?;
        ensure!(
            !invalid_system_texture(&value),
            "unsupported system texture in {name}"
        );
        Ok(value)
    }
    pub fn texture(&self, name: &str, compressed: impl Fn(u32) -> bool) -> Result<Texture> {
        let mut texture = texture::load_compressed(&self.bytes(&texture_name(name)?)?, compressed)
            .with_context(|| format!("reading TEX {name}"))?;
        if texture.frames.is_empty()
            && texture.video.is_none()
            && let Some(metadata) = self.texture_sidecar(name)?
        {
            texture.frames = sheet::frames(&metadata, texture.content)?;
            sheet::normalize(&mut texture.frames, [texture.width, texture.height]);
        }
        Ok(texture)
    }
    pub fn texture_metadata(&self, name: &str) -> Result<(u32, Vec<SpriteFrame>)> {
        let info = self.texture_info(name)?;
        Ok((info.format, info.frames.clone()))
    }
    pub fn texture_info(&self, name: &str) -> Result<std::rc::Rc<TextureInfo>> {
        let key = self.texture_key(name)?;
        if let Some(info) = self.texture_info.borrow().get(&key) {
            return Ok(info.clone());
        }
        let mut info = texture::metadata(&self.bytes(&texture_name(name)?)?)
            .with_context(|| format!("reading TEX metadata {name}"))?;
        if info.frames.is_empty()
            && !info.video
            && let Some(metadata) = self.texture_sidecar(name)?
        {
            info.frames = sheet::frames(&metadata, info.size)?;
        }
        let mut cache = self.texture_info.borrow_mut();
        ensure!(
            cache.len() < 2048
                && cache.values().map(|info| info.frames.len()).sum::<usize>() + info.frames.len()
                    <= 200_000,
            "texture metadata budget exceeded"
        );
        let info = std::rc::Rc::new(info);
        cache.insert(key, info.clone());
        Ok(info)
    }
    fn texture_sidecar(&self, name: &str) -> Result<Option<serde_json::Value>> {
        let key = self.texture_key(name)?;
        let name = format!("{}-json", texture_name(name)?);
        let bytes = match key {
            AssetKey::Package(_) => match self.locate_optional(&name)? {
                Some(AssetKey::Package(name)) => {
                    Some(self.bytes_limit(&name, 8 * 1024 * 1024)?.into_owned())
                }
                Some(AssetKey::File(path)) => Some(bounded_read_limit(&path, 8 * 1024 * 1024)?),
                None => None,
            },
            AssetKey::File(path) => {
                // Metadata follows the resolved TEX file, preserving effect cache scope.
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push("-json");
                match PathBuf::from(sidecar).canonicalize() {
                    Ok(path) => {
                        ensure!(
                            self.roots.iter().any(|root| path.starts_with(root)),
                            "texture JSON escapes root"
                        );
                        Some(bounded_read_limit(&path, 8 * 1024 * 1024)?)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                }
            }
        };
        bytes
            .map(|data| {
                serde_json::from_slice(data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data))
                    .context("reading texture JSON")
            })
            .transpose()
    }
}

fn texture_name(name: &str) -> Result<String> {
    let name = normalize(name)?;
    ensure!(
        !name.starts_with("_rt_"),
        "render target is not an asset: {name}"
    );
    let name = if name.starts_with("materials/") {
        name
    } else {
        format!("materials/{name}")
    };
    Ok(if name.ends_with(".tex") {
        name
    } else {
        format!("{name}.tex")
    })
}

fn invalid_system_texture(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            (object.get("type").and_then(serde_json::Value::as_str) == Some("system")
                && object
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| {
                        !matches!(name, "$mediaThumbnail" | "$mediaPreviousThumbnail")
                    }))
                || object.values().any(invalid_system_texture)
        }
        serde_json::Value::Array(array) => array.iter().any(invalid_system_texture),
        _ => false,
    }
}
#[derive(Clone, Debug)]
pub(crate) struct SpriteFrame {
    /// TEX JSON sequence index; binary TEXS tables form sequence zero.
    pub sequence: usize,
    pub image: usize,
    pub duration: f32,
    pub origin: [f32; 2],
    pub u: [f32; 2],
    pub v: [f32; 2],
    pub pixel_size: [f32; 2],
    pub ratio: f32,
}
pub(crate) fn sequence_range(frames: &[SpriteFrame], sequence: usize) -> Range<usize> {
    let start = frames.partition_point(|frame| frame.sequence < sequence);
    let end = frames.partition_point(|frame| frame.sequence <= sequence);
    start..end
}
pub(crate) struct Texture {
    pub compressed: Option<(u32, Vec<u8>)>,
    pub mipmaps: Vec<TextureMip>,
    pub flags: u32,
    pub video: Option<Vec<u8>>,
    pub frames: Vec<SpriteFrame>,
    pub width: u32,
    pub height: u32,
    pub content: [u32; 2],
    pub rgba: Vec<u8>,
}
pub(crate) struct TextureMip {
    pub size: [u32; 2],
    pub compressed: Option<Vec<u8>>,
    pub rgba: Vec<u8>,
}
pub(crate) struct TextureInfo {
    pub size: [u32; 2],
    pub format: u32,
    pub frames: Vec<SpriteFrame>,
    pub video: bool,
    pub duration: f64,
}
fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    bounded_read_limit(path, LIMIT)
}
fn bounded_read_limit(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = bounded_open(path, limit)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "asset exceeds {} MiB",
        limit / (1024 * 1024)
    );
    Ok(bytes)
}
fn bounded_open(path: &Path, limit: usize) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= limit as u64,
        "asset is not a regular file or exceeds {} MiB",
        limit / (1024 * 1024)
    );
    Ok(file)
}
pub(crate) fn normalize(name: &str) -> Result<String> {
    let name = name.replace('\\', "/");
    let path = Path::new(&name);
    ensure!(
        !name.is_empty()
            && !path.is_absolute()
            && !name.contains(':')
            && !path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))),
        "invalid asset path: {name}"
    );
    Ok(path
        .components()
        .filter_map(|c| match c {
            Component::Normal(v) => v.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

#[cfg(test)]
mod tests {
    use super::tex::read_tex;
    use super::*;
    use std::io::Cursor;
    #[test]
    fn indexed_package_keeps_open_inode_and_rejects_in_place_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("scene.pkg");
        let entries = [
            ("scene.json", b"{\"value\":1}".as_slice()),
            ("effects/test.json", b"{}".as_slice()),
        ];
        let mut bytes = 8u32.to_le_bytes().to_vec();
        bytes.extend(b"PKGV0001");
        bytes.extend((entries.len() as u32).to_le_bytes());
        let mut offset = 0u32;
        for (name, content) in entries {
            bytes.extend((name.len() as u32).to_le_bytes());
            bytes.extend(name.as_bytes());
            bytes.extend(offset.to_le_bytes());
            bytes.extend((content.len() as u32).to_le_bytes());
            offset += content.len() as u32;
        }
        for (_, content) in entries {
            bytes.extend(content);
        }
        std::fs::write(&path, bytes).unwrap();
        let assets = Assets::open(root.path(), &path, None).unwrap();
        let effect = assets.effect("effects/test.json").unwrap();
        assert_eq!(assets.json("scene.json").unwrap()["value"], 1);
        assert_eq!(
            effect.optional_read("effects/test.json").unwrap().unwrap(),
            b"{}"
        );
        assert!(effect.optional_read("missing").unwrap().is_none());
        assert!(assets.read("../scene.json").is_err());
        let old = root.path().join("old.pkg");
        std::fs::rename(&path, &old).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert_eq!(assets.json("scene.json").unwrap()["value"], 1);
        std::fs::write(&old, b"truncated").unwrap();
        assert!(assets.read("scene.json").is_err());
        assert!(effect.read("effects/test.json").is_err());
    }
    #[test]
    fn static_sprite_sidecars_follow_texture_scope_and_reject_escape_fifo_and_budget() {
        let root = tempfile::tempdir().unwrap();
        let scene = root.path().join("scene.json");
        std::fs::write(&scene, b"{}").unwrap();
        let mut tex = b"TEXV0005\0TEXI0001\0".to_vec();
        for n in [0u32, 2, 2, 1, 2, 1, 0] {
            tex.extend(n.to_le_bytes());
        }
        tex.extend(b"TEXB0001\0");
        for n in [1u32, 1, 2, 1, 8] {
            tex.extend(n.to_le_bytes());
        }
        tex.extend([255, 0, 0, 255, 0, 0, 255, 255]);
        let assets = Assets::open(root.path(), &scene, None).unwrap();
        for (effect, count, width) in [("a", 2, 1), ("b", 1, 2)] {
            let directory = root.path().join(effect).join("materials");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("atlas.tex"), &tex).unwrap();
            let mut sequences =
                vec![serde_json::json!({"frames":count,"width":width,"height":1,"duration":2})];
            if effect == "a" {
                sequences.push(serde_json::json!({"frames":1,"width":2,"height":1,"duration":0.5}));
            }
            std::fs::write(
                directory.join("atlas.tex-json"),
                serde_json::to_vec(&serde_json::json!({"spritesheetsequences":sequences})).unwrap(),
            )
            .unwrap();
        }
        let a = assets.effect("a/effect.json").unwrap();
        let b = assets.effect("b/effect.json").unwrap();
        assert_eq!(a.texture_info("atlas").unwrap().frames.len(), 3);
        assert_eq!(b.texture_info("atlas").unwrap().frames.len(), 1);
        assert_eq!(
            a.texture("atlas", |_| false).unwrap().frames[1].origin,
            [0.5, 0.]
        );
        assert_eq!(
            a.texture("atlas", |_| false).unwrap().frames[0].duration,
            1.
        );
        let info = a.texture_info("atlas").unwrap();
        assert_eq!(sequence_range(&info.frames, 1), 2..3);
        assert_eq!(info.frames[2].pixel_size, [2., 1.]);
        let playback = crate::scene::texture_metadata(&info);
        assert_eq!(playback["frameCount"], 2);
        assert_eq!(playback["duration"], 2.);
        assert_eq!(playback["durations"], serde_json::json!([1., 1.]));
        assert!(playback.get("sequence").is_none());
        assert!(playback.get("sequences").is_none());
        assert!(std::rc::Rc::ptr_eq(
            &info,
            &a.texture_info("atlas").unwrap()
        ));
        assert!(!std::rc::Rc::ptr_eq(
            &info,
            &b.texture_info("atlas").unwrap()
        ));
        let sidecar = root.path().join("a/materials/atlas.tex-json");
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(&sidecar).unwrap();
        std::os::unix::fs::symlink(outside.path(), &sidecar).unwrap();
        assert!(a.texture("atlas", |_| false).is_err());
        std::fs::remove_file(&sidecar).unwrap();
        let path = std::ffi::CString::new(sidecar.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(a.texture("atlas", |_| false).is_err());
        std::fs::remove_file(&sidecar).unwrap();
        std::fs::File::create(&sidecar)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        let error = a.texture("atlas", |_| false).err().unwrap();
        assert!(format!("{error:#}").contains("8 MiB"));
    }
    #[test]
    fn unpackaged_bom_scene_and_scoped_package_symlinks_use_the_same_asset_boundary() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("custom.json");
        std::fs::write(&path, b"\xef\xbb\xbf{\"general\":{},\"objects\":[]}").unwrap();
        let assets = Assets::open(root.path(), &path, None).unwrap();
        assert_eq!(
            assets.json("scene.json").unwrap(),
            serde_json::json!({"general":{},"objects":[]})
        );
        std::os::unix::fs::symlink(&path, root.path().join("linked.json")).unwrap();
        assert!(Assets::open(root.path(), &root.path().join("linked.json"), None).is_ok());
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside.json")).unwrap();
        assert!(Assets::open(root.path(), &root.path().join("outside.json"), None).is_err());
    }
    #[test]
    fn corrupt_package_bounds_and_texture_lengths_return_errors() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("scene.pkg");
        let mut pkg = Vec::new();
        pkg.extend(8u32.to_le_bytes());
        pkg.extend(b"PKGV0001");
        pkg.extend(1u32.to_le_bytes());
        pkg.extend(1u32.to_le_bytes());
        pkg.extend(b"x");
        pkg.extend(0u32.to_le_bytes());
        pkg.extend(u32::MAX.to_le_bytes());
        std::fs::write(&path, pkg).unwrap();
        assert!(Assets::open(root.path(), &path, None).is_err());
        let mut tex = b"TEXV0005\0TEXI0001\0".to_vec();
        for n in [0u32, 0, 1, 1, 1, 1, 0] {
            tex.extend(n.to_le_bytes());
        }
        tex.extend(b"TEXB0001\0");
        for n in [1u32, 1, 1, 1, 512 * 1024 * 1024] {
            tex.extend(n.to_le_bytes());
        }
        assert!(read_tex(&mut Cursor::new(tex.as_slice())).is_err());
    }
    #[test]
    fn paths_preserve_spaces_and_reject_escape() {
        assert_eq!(
            normalize("models\\我的 壁纸.json").unwrap(),
            "models/我的 壁纸.json"
        );
        for path in ["../x", "/x", "C:\\x", "a/../../x"] {
            assert!(normalize(path).is_err());
        }
    }
    #[test]
    fn scoped_effect_textures_do_not_alias_in_the_gpu_cache() {
        let root = tempfile::tempdir().unwrap();
        for (effect, byte) in [("a", 1), ("b", 2)] {
            let directory = root.path().join(effect).join("materials");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("shared.tex"), [byte]).unwrap();
        }
        let assets = Assets {
            data: std::rc::Rc::new(PackageData::Json(Vec::new())),
            entries: std::rc::Rc::new(HashMap::new()),
            roots: vec![root.path().canonicalize().unwrap()],
            texture_info: Default::default(),
        };
        let a = assets.effect("a/effect.json").unwrap();
        let b = assets.effect("b/effect.json").unwrap();
        assert_ne!(
            a.texture_key("shared").unwrap(),
            b.texture_key("shared").unwrap()
        );
        assert_eq!(
            a.texture_key("shared").unwrap(),
            a.texture_key(r"materials\shared.tex").unwrap()
        );
        assert_eq!(a.read("materials/shared.tex").unwrap(), [1]);
        assert_eq!(b.read("materials/shared.tex").unwrap(), [2]);
    }
    #[test]
    fn scripts_timelines_and_named_media_textures_are_native() {
        assert!(!invalid_system_texture(
            &serde_json::json!({"visible":false,"alpha":{"script":"x"}})
        ));
        assert!(!invalid_system_texture(
            &serde_json::json!({"passes":[{"animation":{}}]})
        ));
        assert!(!invalid_system_texture(
            &serde_json::json!({"passes":[{"usertextures":[{"type":"system", "name":"$mediaThumbnail"}]}]})
        ));
        assert!(invalid_system_texture(
            &serde_json::json!({"usertextures":[{"type":"system", "name":"$other"}]})
        ));
        assert!(!invalid_system_texture(
            &serde_json::json!({"alpha":{"user":"alpha","value":1}})
        ));
    }
}

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
pub struct Inspection {
    directory: PathBuf,
    engine: Option<&'static str>,
    evidence: Vec<PathBuf>,
    candidates: Vec<PathBuf>,
    selected: Option<PathBuf>,
}

pub fn inspect(directory: &Path) -> Result<Inspection> {
    let directory = fs::canonicalize(directory)
        .with_context(|| format!("找不到游戏目录：{}", directory.display()))?;
    ensure!(directory.is_dir(), "请选择游戏目录");
    ensure!(
        directory.to_str().is_some(),
        "游戏目录必须使用有效的 UTF-8 路径"
    );
    let root = fs::read_dir(&directory).context("无法读取游戏目录")?;
    let mut files = Vec::new();
    let mut candidates = Vec::new();
    for item in root.flatten() {
        let path = item.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_file() {
            if extension(&path).eq_ignore_ascii_case("exe") && launch_candidate(&path) {
                candidates.push(path.clone());
            }
            files.push(path);
        } else if kind.is_dir() && resource_directory(name) {
            if let Ok(children) = fs::read_dir(path) {
                files.extend(children.flatten().filter_map(|child| {
                    let path = child.path();
                    (child.file_type().ok()?.is_file() && path.to_str().is_some()).then_some(path)
                }));
            }
        }
    }
    files.sort_unstable();
    candidates.sort_unstable();

    let mut engines = BTreeMap::new();
    for path in &files {
        if let Some(engine) = archive_engine(path) {
            engines.entry(engine).or_insert_with(|| vec![path]);
        }
    }
    let named = |name: &str| {
        files.iter().find(|path| {
            path.parent() == Some(&directory)
                && path
                    .file_name()
                    .is_some_and(|base| base.eq_ignore_ascii_case(name))
        })
    };
    if let Some(seen) = named("Seen.txt")
        && reallive_archive(seen).is_some()
    {
        let mut evidence = vec![seen];
        evidence.extend(named("Gameexe.ini"));
        engines.insert("RealLive", evidence);
    }
    if let Some(config) = named("Gameexe.dat")
        && let Some(resources) = named("Scene.pck").or_else(|| {
            files.iter().find(|path| {
                extension(path).eq_ignore_ascii_case("g00")
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name.eq_ignore_ascii_case("g00"))
            })
        })
    {
        engines.insert("SiglusEngine", vec![config, resources]);
    }
    if let Some(player) = named("UnityPlayer.dll")
        && pe_flags(player).is_some_and(|flags| flags & 0x2000 != 0)
        && let Some(resources) = files.iter().find(|path| {
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().ends_with("_data"))
                && path.file_name().is_some_and(|name| {
                    name.eq_ignore_ascii_case("boot.config")
                        || name.eq_ignore_ascii_case("globalgamemanagers")
                })
        })
    {
        engines.insert("Unity", vec![player, resources]);
    }
    let nsa = files
        .iter()
        .find(|path| extension(path).eq_ignore_ascii_case("nsa") && nsa_archive(path).is_some());
    if let (Some(script), Some(archive)) = (named("pscript.dat"), nsa) {
        engines.insert("PONScripter", vec![script, archive]);
    } else if let Some(archive) = nsa {
        engines.insert("NScripter", vec![archive]);
    } else if let Some(script) = named("0.txt").or_else(|| named("00.txt"))
        && nscripter_script(script).is_some()
    {
        engines.insert("NScripter", vec![script]);
    }

    let engine = (engines.len() == 1).then(|| *engines.keys().next().unwrap());
    let evidence = engines
        .into_values()
        .flatten()
        .map(|path| path.strip_prefix(&directory).unwrap().to_owned())
        .collect();
    let selected = (candidates.len() == 1).then(|| candidates[0].clone());
    Ok(Inspection {
        directory,
        engine,
        evidence,
        candidates,
        selected,
    })
}

fn extension(path: &Path) -> &str {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
}

fn resource_directory(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(name.as_str(), "archive" | "data" | "game" | "g00" | "pac") || name.ends_with("_data")
}

fn archive_engine(path: &Path) -> Option<&'static str> {
    let (engine, signatures): (_, &[&[u8]]) = match extension(path).to_ascii_lowercase().as_str() {
        "xp3" => ("KiriKiri", &[b"XP3\r\n \n\x1a\x8bg\x01"]),
        "arc" => ("BGI/Ethornell", &[b"BURIKO ARC20", b"PackFile    "]),
        "iga" => ("Noesis", &[b"IGA0"]),
        "ypf" => ("YU-RIS", &[b"YPF\0"]),
        "ain" => ("AliceSoft System 4", &[b"AI2\0", b"VERS"]),
        "int" => ("CatSystem2", &[b"KIF\0"]),
        "rpa" => ("Ren'Py", &[b"RPA-3.0 ", b"RPA-2.0 "]),
        "rpyc" => ("Ren'Py", &[b"RENPY RPC2"]),
        "unity3d" => ("Unity", &[b"UnityFS\0"]),
        _ => return None,
    };
    let mut header = Vec::new();
    File::open(path)
        .ok()?
        .take(16)
        .read_to_end(&mut header)
        .ok()?;
    signatures
        .iter()
        .any(|magic| header.starts_with(magic))
        .then_some(engine)
}

fn nsa_archive(path: &Path) -> Option<()> {
    let mut file = File::open(path).ok()?;
    let mut header = [0; 6];
    file.read_exact(&mut header).ok()?;
    let count = u64::from(u16::from_be_bytes(header[..2].try_into().unwrap()));
    let data = u64::from(u32::from_be_bytes(header[2..].try_into().unwrap()));
    (count > 0 && data >= 6 + count * 14 && data < file.metadata().ok()?.len()).then_some(())
}

fn nscripter_script(path: &Path) -> Option<()> {
    let mut script = Vec::new();
    File::open(path)
        .ok()?
        .take(16 * 1024)
        .read_to_end(&mut script)
        .ok()?;
    let lines = || script.split(|byte| *byte == b'\n').map(<[u8]>::trim_ascii);
    (lines().any(|line| line.eq_ignore_ascii_case(b"*define"))
        && lines().any(|line| line.eq_ignore_ascii_case(b"*start")))
    .then_some(())
}

fn reallive_archive(path: &Path) -> Option<()> {
    // Seen.txt begins with 10,000 little-endian (offset, length) pairs.
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut table = vec![0; 80_000];
    file.read_exact(&mut table).ok()?;
    let offset = table.chunks_exact(8).find_map(|entry| {
        let offset = u64::from(u32::from_le_bytes(entry[..4].try_into().unwrap()));
        let length = u64::from(u32::from_le_bytes(entry[4..].try_into().unwrap()));
        (offset >= table.len() as u64 && length >= 4 && offset + length <= size).then_some(offset)
    })?;
    let mut header = [0; 4];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut header).ok()?;
    matches!(u32::from_le_bytes(header), 0x1cc | 0x1d0).then_some(())
}

fn launch_candidate(path: &Path) -> bool {
    let name = path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_ascii_lowercase();
    if name.starts_with("unins")
        || name.starts_with("unitycrashhandler")
        || matches!(
            name.as_str(),
            "setup"
                | "installer"
                | "config"
                | "settings"
                | "opensavefolder"
                | "dxsetup"
                | "bhvc"
                | "エンジン設定"
                | "セーブファイル設定"
        )
    {
        return false;
    }
    pe_flags(path).is_some_and(|flags| flags & 0x2002 == 0x0002)
}

fn pe_flags(path: &Path) -> Option<u16> {
    let mut file = File::open(path).ok()?;
    let mut dos = [0; 64];
    file.read_exact(&mut dos).ok()?;
    if !dos.starts_with(b"MZ") {
        return None;
    }
    let offset = u64::from(u32::from_le_bytes(dos[60..64].try_into().unwrap()));
    let mut header = [0; 26];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut header).ok()?;
    let flags = u16::from_le_bytes(header[22..24].try_into().unwrap());
    (header.starts_with(b"PE\0\0")
        && matches!(
            u16::from_le_bytes(header[24..].try_into().unwrap()),
            0x10b | 0x20b
        ))
    .then_some(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_resources_by_content_and_keeps_missing_entry_explicit() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("data.xp3"), b"not an archive")?;
        fs::write(dir.path().join("game.exe"), b"MZ")?;
        let result = inspect(dir.path())?;
        assert_eq!(result.engine, None);
        assert!(result.candidates.is_empty());
        fs::write(dir.path().join("data.xp3"), b"XP3\r\n \n\x1a\x8bg\x01")?;
        let result = inspect(dir.path())?;
        assert_eq!(result.engine, Some("KiriKiri"));
        assert_eq!(result.evidence, [PathBuf::from("data.xp3")]);
        assert_eq!(result.selected, None);
        Ok(())
    }

    #[test]
    fn recommends_only_a_unique_game_entry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut exe = [0; 90];
        exe[0..2].copy_from_slice(b"MZ");
        exe[60] = 64;
        exe[64..68].copy_from_slice(b"PE\0\0");
        exe[86] = 2;
        exe[88..90].copy_from_slice(&0x10bu16.to_le_bytes());
        fs::write(dir.path().join("game.exe"), exe)?;
        fs::write(dir.path().join("unins000.exe"), exe)?;
        let first = inspect(dir.path())?;
        assert_eq!(
            first.selected.as_deref(),
            Some(first.directory.join("game.exe").as_path())
        );
        fs::write(dir.path().join("game-chs.EXE"), exe)?;
        let second = inspect(dir.path())?;
        assert_eq!(second.candidates.len(), 2);
        assert_eq!(second.selected, None);
        Ok(())
    }
}

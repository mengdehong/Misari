use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::{process::Command, task::JoinSet};

#[derive(Deserialize)]
struct Results {
    results: Vec<Entry>,
    more: bool,
}

#[derive(Deserialize)]
struct Entry {
    title: String,
    alttitle: Option<String>,
    released: Option<String>,
    #[serde(default, alias = "languages")]
    titles: Vec<LocalizedTitle>,
    #[serde(default)]
    aliases: Vec<String>,
    image: Option<Image>,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    vns: Vec<ReleaseWork>,
}

#[derive(Deserialize)]
struct LocalizedTitle {
    title: Option<String>,
    latin: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseWork {
    rtype: String,
}

#[derive(Deserialize)]
struct Image {
    url: String,
    thumbnail: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

impl Entry {
    fn exact_title(&self, key: &str) -> bool {
        std::iter::once(self.title.as_str())
            .chain(self.alttitle.as_deref())
            .chain(
                self.titles
                    .iter()
                    .flat_map(|title| [title.title.as_deref(), title.latin.as_deref()])
                    .flatten(),
            )
            .any(|name| title_key(name) == key)
    }

    fn exact(&self, key: &str) -> bool {
        self.exact_title(key) || self.aliases.iter().any(|alias| title_key(alias) == key)
    }

    fn cover(&self) -> Option<&Image> {
        self.image.as_ref().or_else(|| {
            let [work] = self.vns.as_slice() else {
                return None;
            };
            if work.rtype != "complete" {
                return None;
            }
            ["pkgfront", "dig"].into_iter().find_map(|kind| {
                self.images
                    .iter()
                    .find(|image| image.kind.as_deref() == Some(kind))
            })
        })
    }
}

fn title_key(title: &str) -> String {
    let folded: String = title
        .chars()
        .map(|ch| {
            if ('！'..='～').contains(&ch) {
                char::from_u32(ch as u32 - 0xfee0).unwrap()
            } else {
                ch
            }
        })
        .flat_map(char::to_lowercase)
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn automatic<'a>(
    title: &str,
    works: &'a Results,
    releases: Option<&'a Results>,
) -> Option<&'a str> {
    let key = title_key(title);
    if let Some(releases) = releases {
        if releases.more {
            return None;
        }
        let mut exact = releases
            .results
            .iter()
            .filter(|release| release.exact(&key));
        if let Some(release) = exact.next() {
            if exact.next().is_some() {
                return None;
            }
            return Some(&release.cover()?.url);
        }
    }
    if works.more {
        return None;
    }
    let mut exact = works.results.iter().filter(|work| work.exact(&key));
    let work = exact.next()?;
    if exact.next().is_some() {
        return None;
    }
    Some(&work.cover()?.url)
}

async fn get(url: &str, query: Option<Value>, limit: &str) -> Result<Vec<u8>> {
    let mut command = Command::new("curl");
    command.kill_on_drop(true).args([
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--proto",
        "=https",
        "--max-time",
        "9",
        "--max-filesize",
        limit,
        "--user-agent",
        concat!("GalScaler/", env!("CARGO_PKG_VERSION")),
    ]);
    if let Some(query) = query {
        command.arg("--json").arg(query.to_string());
    }
    let output = command.arg(url).output().await.context("无法运行 curl")?;
    ensure!(
        output.status.success(),
        "无法读取 VNDB：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

async fn search(kind: &str, filters: Value, fields: &str) -> Result<Results> {
    let query = json!({"filters": filters, "fields": fields, "sort": "searchrank", "results": 30});
    let data = get(
        &format!("https://api.vndb.org/kana/{kind}"),
        Some(query),
        "1048576",
    )
    .await?;
    serde_json::from_slice(&data).context("VNDB 返回了无效结果")
}

async fn lookup(title: &str) -> Result<(Results, Option<Results>)> {
    let key = title_key(title);
    ensure!(
        !key.is_empty() && key.len() <= 512,
        "请填写有效的 VNDB 搜索标题"
    );
    let filter = json!(["search", "=", key]);
    let works = search(
        "vn",
        filter.clone(),
        "title,alttitle,titles{title,latin},aliases,released,image{url,thumbnail}",
    )
    .await?;
    let releases = if works.results.iter().any(|work| work.exact_title(&key)) {
        None
    } else {
        Some(search(
            "release",
            json!(["and", filter, ["platform", "=", "win"], ["patch", "!=", 1]]),
            "title,alttitle,languages{title,latin},released,vns{rtype},images{type,url,thumbnail}",
        ).await?)
    };
    Ok((works, releases))
}

pub async fn find_automatic(data: &Path, title: &str) -> Result<Option<PathBuf>> {
    let (works, releases) = lookup(title).await?;
    match automatic(title, &works, releases.as_ref()) {
        Some(url) => download(data, url).await.map(Some),
        None => Ok(None),
    }
}

#[derive(Serialize)]
pub struct Candidate {
    title: String,
    released: Option<String>,
    release: bool,
    url: String,
    thumbnail: Option<PathBuf>,
}

pub async fn candidates(data: &Path, title: &str) -> Result<Vec<Candidate>> {
    let (works, releases) = lookup(title).await?;
    let key = title_key(title);
    let entries = releases
        .into_iter()
        .flat_map(|results| results.results)
        .filter(|entry| entry.exact(&key))
        .map(|entry| (entry, true))
        .chain(works.results.into_iter().map(|entry| (entry, false)));
    let mut previews = JoinSet::new();
    let mut urls = std::collections::HashSet::new();
    for (index, (entry, release)) in entries
        .filter(|(entry, _)| {
            entry
                .cover()
                .is_some_and(|image| urls.insert(image.url.clone()))
        })
        .take(6)
        .enumerate()
    {
        let data = data.to_owned();
        previews.spawn(async move {
            let image = entry.cover().unwrap();
            let thumbnail = match &image.thumbnail {
                Some(url) => download(&data, url).await.ok(),
                None => None,
            };
            let url = image.url.clone();
            (
                index,
                Candidate {
                    title: entry
                        .alttitle
                        .filter(|alt| alt != &entry.title)
                        .map(|alt| format!("{alt} · {}", entry.title))
                        .unwrap_or(entry.title),
                    released: entry.released,
                    release,
                    url,
                    thumbnail,
                },
            )
        });
    }
    let mut candidates = previews.join_all().await;
    candidates.sort_unstable_by_key(|(index, _)| *index);
    Ok(candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect())
}

pub fn read(path: &Path) -> Result<(&'static str, Vec<u8>)> {
    const LIMIT: u64 = 6 * 1024 * 1024;
    let info = path.metadata()?;
    ensure!(info.is_file() && info.len() <= LIMIT, "无效的封面文件");
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= LIMIT, "封面文件过大");
    let media_type = match bytes.as_slice() {
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => "image/png",
        [b'R', b'I', b'F', b'F', ..] if bytes.get(8..12) == Some(b"WEBP") => "image/webp",
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => "image/gif",
        [0, 0, 1, 0, ..] => "image/x-icon",
        [b'B', b'M', ..] => "image/bmp",
        _ => anyhow::bail!("不支持的封面格式"),
    };
    Ok((media_type, bytes))
}

pub async fn download(data: &Path, url: &str) -> Result<PathBuf> {
    let image = url
        .strip_prefix("https://t.vndb.org/")
        .context("无效的 VNDB 图片地址")?;
    let mut parts = image.split('/');
    let (Some(kind), Some(bucket), Some(name), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        anyhow::bail!("无效的 VNDB 图片地址");
    };
    let id = name.strip_suffix(".jpg").context("无效的 VNDB 图片格式")?;
    ensure!(
        matches!(kind, "cv" | "cv.t" | "ri" | "ri.t")
            && bucket.len() == 2
            && bucket.bytes().all(|b| b.is_ascii_digit())
            && !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit()),
        "无效的 VNDB 图片地址"
    );
    let dir = data.join("covers");
    let path = dir.join(format!("{kind}-{name}"));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = get(url, None, "6291456").await?;
    ensure!(
        bytes.starts_with(b"\xff\xd8\xff") && bytes.ends_with(b"\xff\xd9"),
        "VNDB 返回了无效的封面图片"
    );
    fs::create_dir_all(&dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(&bytes)?;
    file.persist(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn automatically_uses_only_a_unique_exact_title() -> anyhow::Result<()> {
        let mut works = serde_json::from_value::<Results>(json!({"more": false, "results": [
            {"title": "Summer Pockets Sunnyside Stories"},
            {"title": "Summer Pockets", "aliases": ["夏日口袋"],
             "image": {"url": "work-cover"}}
        ]}))?;
        assert_eq!(
            automatic("Ｓｕｍｍｅｒ　Ｐｏｃｋｅｔｓ", &works, None),
            Some("work-cover")
        );
        assert_eq!(automatic("夏日口袋", &works, None), Some("work-cover"));
        assert_eq!(automatic("Summer", &works, None), None);

        works
            .results
            .push(serde_json::from_value(json!({"title": "夏日口袋"}))?);
        assert_eq!(automatic("夏日口袋", &works, None), None);
        Ok(())
    }

    #[test]
    fn edition_lookup_requires_its_own_unambiguous_cover() -> Result<()> {
        let title = "Game Deluxe";
        let works = serde_json::from_value(json!({"more": false, "results": [
            {"title": "Game", "aliases": [title], "image": {"url": "base-cover"}}
        ]}))?;
        let mut releases = serde_json::from_value::<Results>(json!({"more": false, "results": [
            {"title": title, "vns": [{"rtype": "complete"}],
             "images": [{"type": "pkgback", "url": "back"}, {"type": "pkgfront", "url": "front"}]}
        ]}))?;
        assert_eq!(automatic(title, &works, Some(&releases)), Some("front"));
        releases.more = true;
        assert_eq!(automatic(title, &works, Some(&releases)), None);
        releases.more = false;
        releases.results[0].images.clear();
        assert_eq!(automatic(title, &works, Some(&releases)), None);
        Ok(())
    }
}

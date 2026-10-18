use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Kind,
    Feature,
    Rating,
    Resolution,
    Tag,
}

pub const GROUPS: [Group; 5] = [
    Group::Kind,
    Group::Feature,
    Group::Rating,
    Group::Resolution,
    Group::Tag,
];

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Self::Kind => "类型",
            Self::Feature => "功能",
            Self::Rating => "年龄分级",
            Self::Resolution => "分辨率",
            Self::Tag => "标签",
        }
    }

    pub fn options(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Kind => &[
                ("Scene", "场景"),
                ("Video", "视频"),
                ("Web", "网页"),
                ("Application", "应用"),
                ("Image", "图片"),
                ("Shader", "着色器"),
            ],
            Self::Feature => &[
                ("Approved", "广受好评"),
                ("Audio responsive", "音频响应"),
                ("Customizable", "可自定义"),
            ],
            Self::Rating => &[
                ("Everyone", "大众级 (G)"),
                ("Questionable", "家长指导级 (PG-13)"),
                ("Mature", "限制级 (R-18)"),
            ],
            Self::Resolution => &[
                ("1280 x 720", "1280 × 720"),
                ("1920 x 1080", "1920 × 1080"),
                ("2560 x 1440", "2560 × 1440"),
                ("3840 x 2160", "3840 × 2160"),
                ("Ultrawide 2560 x 1080", "超宽屏 2560 × 1080"),
                ("Ultrawide 3440 x 1440", "超宽屏 3440 × 1440"),
                ("Portrait 1080 x 1920", "竖屏 1080 × 1920"),
                ("Portrait 1440 x 2560", "竖屏 1440 × 2560"),
                ("Portrait 2160 x 3840", "竖屏 2160 × 3840"),
                ("Other resolution", "其他分辨率"),
                ("Dynamic resolution", "动态分辨率"),
            ],
            Self::Tag => &[
                ("Abstract", "抽象"),
                ("Animal", "动物"),
                ("Anime", "动漫"),
                ("Cartoon", "卡通"),
                ("CGI", "CGI"),
                ("Cyberpunk", "赛博朋克"),
                ("Fantasy", "幻想"),
                ("Game", "游戏"),
                ("Girls", "女性"),
                ("Guys", "男性"),
                ("Landscape", "风景"),
                ("Medieval", "中世纪"),
                ("Memes", "网络事物"),
                ("MMD", "MMD"),
                ("Music", "音乐"),
                ("Nature", "自然"),
                ("Pixel art", "像素艺术"),
                ("Relaxing", "放松"),
                ("Retro", "复古"),
                ("Sci-Fi", "科幻"),
                ("Sports", "运动"),
                ("Technology", "科技"),
                ("Television", "电视节目"),
                ("Vehicle", "汽车"),
                ("Unspecified", "未指定样式"),
            ],
        }
    }

    pub fn label(self, value: &str) -> &str {
        self.options()
            .iter()
            .find(|(key, _)| *key == value)
            .map(|(_, label)| *label)
            .unwrap_or(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filters {
    #[serde(default)]
    pub selected: BTreeMap<Group, BTreeSet<String>>,
}

impl Filters {
    pub fn from_action(action: &Value) -> Result<Self, String> {
        let filters: Self = if action["filters"].is_null() {
            Self::default()
        } else {
            serde_json::from_value(action["filters"].clone()).map_err(|_| "筛选条件格式无效。")?
        };
        for (group, values) in &filters.selected {
            if values
                .iter()
                .any(|value| !group.options().iter().any(|(key, _)| key == value))
            {
                return Err("筛选条件包含未知选项。".into());
            }
        }
        Ok(filters)
    }

    pub fn contains(&self, group: Group, value: &str) -> bool {
        self.selected
            .get(&group)
            .is_some_and(|values| values.contains(value))
    }

    pub fn set(&mut self, group: Group, value: String, enabled: bool) {
        if enabled {
            self.selected.entry(group).or_default().insert(value);
        } else if let Some(values) = self.selected.get_mut(&group) {
            values.remove(&value);
            if values.is_empty() {
                self.selected.remove(&group);
            }
        }
    }

    pub fn count(&self) -> usize {
        self.selected
            .values()
            .filter(|values| !values.is_empty())
            .count()
    }

    /// Alternatives inside a group; features are cumulative requirements.
    pub fn tag_groups(&self) -> Vec<Vec<&str>> {
        self.selected
            .iter()
            .filter(|(_, values)| !values.is_empty())
            .flat_map(|(group, values)| {
                if *group == Group::Feature {
                    values
                        .iter()
                        .map(|value| vec![value.as_str()])
                        .collect::<Vec<_>>()
                } else {
                    vec![values.iter().map(String::as_str).collect()]
                }
            })
            .collect()
    }

    pub fn matches(&self, item: &Value) -> bool {
        if self.count() == 0 {
            return true;
        }
        let tags = metadata_tags(item);
        self.selected.iter().all(|(group, values)| {
            let has = |value: &String| tags.iter().any(|tag| tag.eq_ignore_ascii_case(value));
            values.is_empty()
                || if *group == Group::Feature {
                    values.iter().all(has)
                } else {
                    values.iter().any(has)
                }
        })
    }
}

pub fn resolution_tag(width: u32, height: u32) -> String {
    let dimensions = format!("{width} x {height}");
    Group::Resolution
        .options()
        .iter()
        .find(|(key, _)| key.ends_with(&dimensions))
        .map(|(key, _)| *key)
        .unwrap_or("Other resolution")
        .into()
}

pub fn metadata_tags(item: &Value) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    for value in crate::array(&item["tags"])
        .iter()
        .chain(crate::array(&item["project"]["tags"]))
    {
        if let Some(tag) = value.as_str().or_else(|| value["tag"].as_str()) {
            tags.insert(tag.to_owned());
        }
    }
    let kind = match crate::strv(item, "kind") {
        "scene" | "we_scene" => "Scene",
        "video" => "Video",
        "web" | "we_web" => "Web",
        "application" | "we_application" => "Application",
        "image" => "Image",
        "shader" => "Shader",
        _ => "",
    };
    if !kind.is_empty() {
        tags.insert(kind.into());
    }
    let project = &item["project"];
    for value in [&item["contentrating"], &project["contentrating"]] {
        if let Some(rating) = value.as_str() {
            tags.insert(rating.into());
        }
    }
    if project["general"]["supportsaudioprocessing"] == true {
        tags.insert("Audio responsive".into());
    }
    if project["general"]["properties"]
        .as_object()
        .is_some_and(|properties| !properties.is_empty())
    {
        tags.insert("Customizable".into());
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn groups_match_alternatives_features_and_unknown_metadata() {
        let item = json!({"kind":"we_scene","project":{"tags":["Landscape"],"contentrating":"Everyone","general":{"supportsaudioprocessing":true,"properties":{"speed":{}}}}});
        let mut filters = Filters::default();
        for (group, value) in [
            (Group::Kind, "Scene"),
            (Group::Kind, "Video"),
            (Group::Tag, "Landscape"),
            (Group::Rating, "Everyone"),
            (Group::Feature, "Audio responsive"),
            (Group::Feature, "Customizable"),
        ] {
            filters.set(group, value.into(), true);
        }
        assert!(filters.matches(&item));
        assert!(!filters.matches(&json!({"kind":"video","tags":["Landscape"]})));
        filters.set(Group::Tag, "Landscape".into(), false);
        filters.set(Group::Tag, "Nature".into(), true);
        assert!(!filters.matches(&item));
        assert!(Filters::default().matches(&json!({"kind":"image"})));
        assert!(
            Filters::from_action(&json!({"filters":{"selected":{"tag":["invalid\u{0000}"]}}}))
                .is_err()
        );
    }
}

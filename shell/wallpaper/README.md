# Wallpaper

Noctalia v5 壁纸插件，通过 wallpaperd 浏览、整理和播放壁纸。需要 Noctalia plugin API 24 与支持 `list --thumbnails`、`library`、`library-edit`、`rotation` 的 wallpaperd（需先启动）。文件选择器用可选的 `zenity`，也可直接粘贴路径导入。

## 启用

```sh
noctalia msg plugins source add misari path "$PWD/shell"
noctalia msg plugins enable misari/wallpaper
noctalia msg panel-toggle misari/wallpaper:browser
```

栏组件类型为 `misari/wallpaper:wallpaper`。插件默认同步概览与锁屏背景，可在 Noctalia 设置 → 插件 → Wallpaper 的齿轮中关闭；使用概览背景需在 niri 配置中加 `layer-rule { match namespace="^noctalia-backdrop$"; place-within-backdrop true }`，并合入 [backgrounds.toml](backgrounds.toml)：

```sh
install -Dm644 shell/wallpaper/backgrounds.toml ~/.config/noctalia/zz-wallpaperd-backgrounds.toml
```

## 使用

- **换壁纸**：左侧浏览或搜索 → 点卡片 → 应用；工具栏调整来源、排序、视图，多显示器可在侧栏切换。
- **收藏／播放列表**：点卡片心形收藏；侧栏列表旁 `+` 命名后勾选成员，可排序、重命名、删除。
- **自动轮播**：选显示器 → 打开收藏或列表 → 选顺序／随机及间隔 → 开启；收藏按名称、列表按手动排列，隐藏和不可用素材跳过。
- **管理壁纸**：侧栏“设置” → “壁纸管理”，添加文件、目录或粘贴路径；垃圾桶图标展开确认后移除来源。
- **WE 属性**：场景详情“属性”页修改开关、滑块、颜色、文字并应用；“信息”页含作者说明与链接。

`Ctrl+F` 聚焦搜索，`Ctrl+R` 刷新，`Esc` 关闭。

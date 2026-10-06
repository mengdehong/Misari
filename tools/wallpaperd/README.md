# wallpaperd

适用于 niri / Wayland 的动态壁纸服务。一条命令即可把图片、视频、GIF、GLSL shader 或 Wallpaper Engine（WE）壁纸设为桌面背景。

## 特点

- **全类型壁纸**：图片、视频、GIF、Shadertoy 风格 GLSL shader，以及 Wallpaper Engine 的 video / scene / web 三种项目。
- **多显示器友好**：每块屏幕独立运行、独立设置壁纸；也可以一条命令作用于全部屏幕。
- **切换平滑稳定**：支持 `fade`、`disc`、`honeycomb`、`spiral`、`stripes` 等转场动画；新壁纸加载失败时保留旧画面，不会黑屏。
- **省电省资源**：锁屏、休眠、当前工作区有全屏窗口时自动暂停；别的程序在发声时自动静音壁纸。
- **自动轮播**：可对收藏或自建播放列表按顺序/随机定时切换。
- **素材库**：自动扫描壁纸目录和 Steam Workshop，支持收藏、播放列表、缩略图。
- **WE 壁纸可调**：可实时修改 WE 壁纸的自定义属性（颜色、开关、滑块等），重启后依然保留。
- **会话级服务**：随图形会话启动和退出，重启或重新接入显示器后自动恢复上次的壁纸。
- **图形界面可选**：搭配 [Noctalia 壁纸插件](../../shell/wallpaper/README.md) 即可在面板里点选壁纸、管理收藏和轮播。

## 快速上手

### 1. 准备环境

- 运行 niri Wayland Session。
- 播放视频/GIF 需要系统装有 `libmpv`（`libmpv.so.2`）。
- 显卡驱动需支持 EGL / GLES 3（转场动画、shader、WE scene 都依赖它）。没有 EGL 时只能显示图片。
- 从源码编译需要：Rust 1.96+、pkg-config、C/C++ 编译器，以及 Wayland、libpulse、shaderc 开发文件。

### 2. 编译并安装

```sh
cd tools/wallpaperd
cargo build --locked --release

install -Dm755 target/release/wallpaperd ~/.local/bin/wallpaperd
install -Dm644 configs/wallpaperd.service ~/.config/systemd/user/wallpaperd.service
```

确认 `~/.local/bin` 在你的 `PATH` 中。

### 3. 启动服务

先确保 systemd 用户服务能拿到图形会话的环境变量。通常在 niri 配置里（或会话启动脚本中）加一行：

```sh
systemctl --user import-environment WAYLAND_DISPLAY XDG_RUNTIME_DIR NIRI_SOCKET
dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_RUNTIME_DIR NIRI_SOCKET
```

然后启用服务：

```sh
systemctl --user daemon-reload
systemctl --user enable --now wallpaperd.service
```

> 只想先试一下？可以不装服务，在一个终端里运行 `wallpaperd serve`，在另一个终端里发命令。

### 4. 设置第一张壁纸

```sh
wallpaperd status                       # 查看屏幕名称（如 DP-1）
wallpaperd set ~/Pictures/wallpaper.jpg
```

不加 `--output` 就作用于所有屏幕。常见用法：

```sh
# 视频壁纸，限制 30 帧，淡入切换
wallpaperd set ~/Videos/loop.mp4 --output DP-1 --fps 30 --transition fade

# 暂停 / 恢复 / 调整音量
wallpaperd pause
wallpaperd resume
wallpaperd playback --mute false --volume 50

# 清除壁纸
wallpaperd release --output DP-1
```

| 选项 | 说明 | 默认 |
| --- | --- | --- |
| `--output` | 指定屏幕，省略为全部 | 全部 |
| `--fit` | `cover` 铺满裁切 / `contain` 完整显示 / `stretch` 拉伸 | `cover` |
| `--transition` | `cut` `fade` `disc` `honeycomb` `spiral` `stripes`（动画 1 秒，需 EGL） | `cut` |
| `--fps` | 帧率上限（1–240） | 30 |
| `--mute` / `--volume` | 静音 / 音量 | 静音 / 100 |

视频与 GIF 自动循环。

## 找到并使用你的壁纸

把壁纸放进目录（例如 `~/Pictures/Wallpapers`），在配置里声明（见下节），然后：

```sh
wallpaperd list
```

```text
NAME      TYPE   PATH
lake.jpg  image  ~/Pictures/Wallpapers/lake.jpg
星空      scene  ~/Steam/steamapps/workshop/content/431960/123
```

`PATH` 列可以直接交给 `wallpaperd set`：

```sh
wallpaperd set local:~/Pictures/Wallpapers/lake.jpg
```

Steam Workshop 中订阅的 Wallpaper Engine 壁纸（素材目录 431960）会被自动发现。

## 配置（可选）

配置文件：`~/.config/misari/wallpaperd.toml`，修改后重启服务生效（`systemctl --user restart wallpaperd`）。路径支持绝对路径或 `~/` 开头。

```toml
asset_dirs = ["~/Pictures/Wallpapers"]    # 壁纸目录，供 list 扫描
default = "~/Pictures/Wallpapers/default.jpg"  # 没有已保存选择时使用
fit = "cover"
transition = "fade"
pause_on_session = true      # 锁屏/休眠时暂停
pause_on_fullscreen = true   # 有全屏窗口时暂停
mute_on_other_audio = true   # 其他程序发声时静音
media_integration = true     # 把正在播放的音乐信息传给支持的 WE 壁纸

[playback]
fps = 30
mute = true
volume = 100

[wallpaper_engine]
# assets = "/path/to/Steam/steamapps/common/wallpaper_engine/assets"

# 为某块屏幕单独设置（名称见 wallpaperd status）
[outputs."DP-1"]
default = "~/Pictures/Wallpapers/portrait.jpg"
fit = "contain"
```

壁纸选择会保存在 `~/.local/state/misari/wallpaperd/`，重启后自动恢复；你手动 `release` 的屏幕会保持空白。命令行里显式给出的 `--fit`、`--transition` 优先于配置。

## 自动轮播与收藏

建议通过 UI 插件配置

手动操作：

```sh
# 导入目录、收藏、建播放列表
wallpaperd library-edit '{"action":"import","path":"/home/you/Pictures/Wallpapers"}'
wallpaperd library-edit '{"action":"favorite","asset_id":"local:/home/you/Pictures/Wallpapers/lake.jpg","favorite":true}'
wallpaperd library-edit '{"action":"create_playlist","name":"工作"}'      # 返回新列表 ID，如 playlist:1
wallpaperd library-edit '{"action":"playlist_members","id":"playlist:1","members":["local:/home/you/Pictures/Wallpapers/lake.jpg"]}'

# 每 5 分钟顺序轮播 playlist:1
wallpaperd rotation '{"source":"playlist:1","enabled":true,"mode":"ordered","interval_seconds":300}' --output DP-1

# 立刻切下一张
wallpaperd next --output DP-1
```

## Wallpaper Engine 壁纸

支持传入项目目录、`project.json` 或 `we:/绝对路径`：

```sh
wallpaperd set ~/.local/share/Steam/steamapps/workshop/content/431960/123456789
```

| 类型 | 说明 |
| --- | --- |
| video | 直接用 libmpv 播放，无需 WE 本体或公共 assets |
| scene | 内置渲染器，支持粒子、模型、文字、脚本、音频响应、光照等 |
| web | 通过 Chromium（CEF）渲染，需额外安装 Web 运行库 |

### 调整壁纸属性

```sh
wallpaperd properties <项目路径>                          # 查看可调属性和当前值
wallpaperd set-properties <项目路径> '{"musicbar":false}'   # 修改
wallpaperd set-properties <项目路径> '{"musicbar":null}'    # 恢复默认
```

颜色用 `[r,g,b]` 或 `"r g b"`（0–1 浮点），开关用布尔值，滑块用数字。修改会被保存。

### 启用 Web 壁纸（可选）

Web 壁纸依赖 CEF，默认构建不包含。需要时用带 `web` 特性的版本重新编译并安装运行库（首次编译会自动下载 CEF）：

```sh
cargo build --locked --features web --release --target-dir target/we-web

mkdir -p "$HOME/.local/lib/wallpaperd/web"
cp -a target/we-web/release/lib*.so* \
  target/we-web/release/*.pak target/we-web/release/*.bin target/we-web/release/*.dat \
  target/we-web/release/*.json target/we-web/release/chrome-sandbox \
  target/we-web/release/locales \
  "$HOME/.local/lib/wallpaperd/web/"
install -Dm755 target/we-web/release/wallpaperd "$HOME/.local/bin/wallpaperd"
systemctl --user restart wallpaperd
```

只有第一次加载 Web 壁纸时才会载入 CEF，没用到时不占额外内存。支持 HTML5/Canvas/WebGL、音频可视化、鼠标交互和音乐播放信息。

## 其他功能

- **鼠标交互**：壁纸可以接收鼠标事件（适用于 shader、scene 和 web 壁纸），坐标会自动适配缩放与旋转。
- **多屏同步**：多块屏幕播放同一个视频、shader 或 scene 时，进度自动同步（Web 壁纸除外）。
- **导出当前画面**：`wallpaperd snapshot --output DP-1` 输出当前壁纸的 PNG，可用于锁屏或概览背景。
- **音乐联动**：通过 MPRIS 把歌名、封面、播放进度传给支持的 scene/web 壁纸；设置 `media_integration = false` 可关闭。
- **事件订阅**：`wallpaperd subscribe` 持续输出状态变化，方便脚本使用。

## 自己写 shader 壁纸

保存为 `.frag` 文件后用 `wallpaperd set` 即可。写法类似 Shadertoy：只需提供 `mainImage`，无需 `#version`、`main` 和 uniform 声明；单 pass，不支持 `iChannel`，文件不超过 1 MiB。

```glsl
void mainImage(out vec4 color, in vec2 fragCoord) {
    color = vec4(fragCoord / iResolution.xy, 0.5 + 0.5 * sin(iTime), 1.0);
}
```

可用变量：`iTime`（播放秒数，暂停时不走）、`iResolution`（像素宽高）、`iFrame`（帧序号）、`iMouse`（鼠标位置与点击）。

## 常见问题

- **`set` 后没反应 / 提示连接失败**：服务没启动。执行 `systemctl --user status wallpaperd`，或 `journalctl --user -u wallpaperd -e` 查看原因，多半是 systemd 用户环境缺少 `WAYLAND_DISPLAY` 等变量（见第 3 步）。
- **视频无法播放**：确认系统已安装 `libmpv`（`libmpv.so.2`）。
- **转场/shader/WE scene 不工作**：需要 EGL 1.5 与 GLES 3，检查显卡驱动。
- **Web 壁纸打不开**：确认已按上文安装 Web 运行库到 `~/.local/lib/wallpaperd/web/`。
- **找不到屏幕名称**：运行 `wallpaperd status`。

## Wallpaper Engine 实现方案对比

包含：

- [Wine 桥接（we-layerd）](https://github.com/Aromatic05/we-layerd/tree/a744410164c6ca5c26c38f73f53ead355fa064e4)：v1-last-main（`a744410`）；scene/video 使用 GE-Proton 11-7 + DXVK，web 使用 Wine 11.18；均启用 gamescope headless。
- [LWE（linux-wallpaperengine）](https://github.com/Almamu/linux-wallpaperengine)：b016d7d1 + web 纹理 ID 单行修复
- [Waywallen](https://github.com/waywallen/waywallen) 0.4.3 + [OWE（Open Wallpaper Engine）](https://github.com/waywallen/open-wallpaper-engine) 0.3.0
- [we-layerd](https://github.com/Aromatic05/we-layerd) 0.2.9
- [wallpaperd（Misari）](https://github.com/mengdehong/Misari)


### 功能对比矩阵

| 功能 | [Wine 桥接](https://github.com/Aromatic05/we-layerd/tree/a744410164c6ca5c26c38f73f53ead355fa064e4) | [LWE](https://github.com/Almamu/linux-wallpaperengine) | [Waywallen](https://github.com/waywallen/waywallen) | [we-layerd](https://github.com/Aromatic05/we-layerd) | [wallpaperd](https://github.com/mengdehong/Misari) |
| --- | --- | --- | --- | --- | --- |
| WE 场景壁纸 | 支持 | 支持 | 需 OWE 插件 | 支持 | 支持 |
| WE 视频壁纸 | 支持 | 支持 | 支持 | 支持 | 支持 |
| WE 网页壁纸 | 支持 | 需修复纹理 ID | 需 OWE 插件 | 支持 | 支持 |
| 需要 Windows 版 WE | 需要 | 不需要 | 不需要 | 不需要 | 不需要 |
| 需要捕获 X11 窗口 | 需要 | 不需要 | 不需要 | 不需要 | 不需要 |
| 转发 Wayland 鼠标输入 | 不支持 | 支持 | 支持 | 支持 | 支持 |
| 传入 Linux 桌面音频频谱 | 不支持 | 支持 | 支持 | 支持 | 支持 |
| 传入 Linux 音乐播放信息 | 不支持 | 支持 | 支持 | 仅场景壁纸 | 支持 |
| 从 Linux 端修改壁纸属性 | 不支持 | 启动时设置 | 播放中修改 | 播放中修改 | 播放中修改 |
| 各屏幕设置不同壁纸 | 需映射 WE 窗口 | 支持 | 支持 | 支持 | 支持 |
| 后台按播放列表定时切换 | 不支持 | 不支持 | 支持 | 支持 | 支持 |
| 图形界面 | 自带 | 需第三方前端 | 自带 | 自带 | 自带 Noctalia 插件 |
| 有全屏窗口时自动暂停 | 不支持 | 需合成器协议 | 需桌面集成 | 需合成器协议 | 支持 niri |
| 其他程序发声时自动静音 | 不支持 | 支持 | 支持 | 不支持 | 支持 |


### Benchmark

测试配置

- 软件环境：Arch Linux / Misari / Mesa：26.2.3
- GPU：RX 9070 XT
- 输出：3840×2160（4K）/60 FPS

测试素材：

- scene：【Customize自定义】Hatsune Miku 初音未来 星河沉梦——夜莺Night   Starry River Sinking Dreams
- video：大和撫子 4K 60FPS 无缝循环
- web：今汐 鸣潮 Wuthering Waves

对比结果

```text
./tools/wallpaperd/scripts/benchmark.py --bench --headless --fps 60 --warmup 15 --duration 30

+-------+---------+--------+---------+-------------+-------------+--------------+
| Type  | Metrics | 1 Wine | 2 LWE   | 3 waywallen | 4 we-layerd | 5 wallpaperd |
+-------+---------+--------+---------+-------------+-------------+--------------+
| scene | FPS     |   58.1 |    56.0 |        59.6 |        59.7 |         60.0 |
|       | CPU%    |   69.2 |     7.9 |        24.6 |        14.2 |         32.1 |
|       | PSS     |  890.7 |   270.6 |       358.6 |       154.7 |        197.7 |
|       | VRAM    | 1434.3 |   631.6 |       752.3 |       586.9 |        547.7 |
|       | GPU%    |   26.3 |     6.4 |        10.8 |         9.0 |         10.0 |
|       | Status  |    RUN | PARTIAL |         RUN |     PARTIAL |          RUN |
+-------+---------+--------+---------+-------------+-------------+--------------+
| video | FPS     |   59.0 |    51.2 |        59.9 |        55.3 |         59.8 |
|       | CPU%    |  161.8 |    14.9 |         5.3 |       147.8 |          9.6 |
|       | PSS     |  655.6 |   308.7 |       176.9 |       175.0 |        175.3 |
|       | VRAM    | 1422.7 |   593.7 |       649.6 |       182.0 |        432.6 |
|       | GPU%    |   18.9 |     3.8 |         1.6 |         N/A |          4.2 |
|       | Status  |    RUN |     RUN |         RUN |         RUN |          RUN |
+-------+---------+--------+---------+-------------+-------------+--------------+
| web   | FPS     |   59.4 |    43.4 |        60.0 |        58.2 |         60.0 |
|       | CPU%    |  126.6 |   101.4 |        36.1 |        35.5 |         44.6 |
|       | PSS     | 2728.8 |   647.3 |       567.3 |       442.9 |        470.7 |
|       | VRAM    | 1265.5 |   949.7 |       493.1 |       785.4 |        809.6 |
|       | GPU%    |   19.9 |    19.7 |         3.0 |         6.2 |          6.1 |
|       | Status  |    RUN |     RUN |         RUN |         RUN |          RUN |
+-------+---------+--------+---------+-------------+-------------+--------------+

FPS: avg (Wayland buffer commits/s)
CPU%: avg; one core = 100%
PSS / VRAM: avg, MiB
GPU%: avg, process 3D engine usage
```

各方案在独立 headless niri 中串行运行，Weston kiosk shell 将嵌套 niri 全屏至 3840×2160，并通过截图校验实际输出分辨率。预热 15s、采样 30s，间隔 1s；平均值按采样时长加权。

RUN：有画面且动画；PARTIAL：有画面但部分内容缺失；BLACK：黑屏；SKIP：跳过；N/A：无有效数据或指标不可用。

- scene：LWE 缺少部分星轨和文字；we-layerd 缺少圆环模型。
- video：Wine 使用 WE 的 `mfEngine`，硬件加速配置开启。we-layerd 使用 SHM，GPU% 未采集到。
- Waywallen scene/web 内部渲染为 1920×1080，最终输出为 3840×2160。
- wallpaperd web 使用 `WALLPAPERD_WEB_DIRECT=1`，本表各项均为单次采样。

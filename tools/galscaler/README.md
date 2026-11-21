# GalScaler

GalScaler 在 niri 等 Linux Wayland 桌面上管理和启动 Windows Galgame。它通过 umu 和 Proton 运行游戏，支持实时画质增强，也可以把游戏串流到同一局域网内的浏览器或 Android 客户端。

## 构建与启动

- **运行游戏**需要 systemd 用户会话、`umu-run` 和已安装的 Proton 或 GE-Proton。桌面增强还需要 Wayland、Xwayland 和支持 EGL/GLES 的显卡驱动。通过 VNDB 查找封面需要 `curl`。
- **构建后端**需要 Rust 1.96+、C 编译器、`pkg-config`、`winegcc`、Clang 和 LLD，以及 libplacebo 7+、EGL/GLES、Wayland、libxkbcommon、FFmpeg（libavcodec、libavutil、libswscale）、libva、libdrm、Opus 和 libdatachannel 的开发文件。
- **构建桌面或 Android 客户端**需要 Go 1.27.3+。

在仓库根目录运行以下命令。桌面客户端会自动连接后端，必要时启动它。

```sh
cd tools/galscaler
./ui/run
```

只使用 CLI 时，在 `tools/galscaler` 构建并检查环境。命令默认输出 JSON。

```sh
cargo build --locked --release
./target/release/galscaler doctor
./target/release/galscaler runtimes
```

## 游戏库

在桌面端选择游戏目录或 `.exe` 文件。目录扫描会识别常见视觉小说引擎的资源和候选入口。有唯一候选时会推荐该入口。标题默认取游戏目录名，封面可使用本地图片或从 VNDB 选择。

每个游戏可以覆盖全局的 Proton、环境变量和画质设置，也可单独设置启动参数。新游戏默认使用共享 Wine 环境。共享环境在首次启动时绑定 Proton 版本，同一时间只运行一个游戏。需要不同 Proton 版本或单独存档环境时，选择独立环境。

下面用 CLI 添加、启动和查询游戏。将 `id` 替换为 `add` 返回的游戏 ID。

```sh
./target/release/galscaler add "/path/to/game.exe" --name "游戏名称"
id=1
./target/release/galscaler edit "$id" --prefix independent --filter anime4k
./target/release/galscaler launch "$id"
./target/release/galscaler status
./target/release/galscaler log "$id" --lines 100
```

`edit --arguments` 用 JSON 数组替换启动参数，`edit --environment` 用 JSON 对象替换环境变量。传入 `[]` 可清空启动参数，传入 `{}` 可让该游戏使用空的环境变量覆盖项。修改在下次启动游戏时生效。

关闭 GalScaler 桌面界面后，后端和游戏继续运行。`stop <id>` 结束单个会话，`shutdown` 结束全部会话并关闭后端。停止游戏后可用 `remove <id>` 移出游戏库，游戏文件和 Wine 环境会保留。其他命令见 `galscaler --help`。

## 画质增强

默认用 CuNNy 将 1280×720 的虚拟屏幕画面增强到 2560×1440 的目标范围，以窗口模式运行，帧率设置为 60。也可选择 Anime4K 或基础缩放，对应 CLI 的 `cunny`、`anime4k` 和 `linear`。关闭增强后，桌面端直接通过 umu 和 Proton 启动游戏。

- `source` 是游戏看到的虚拟屏幕尺寸。
- `output` 是增强处理的目标尺寸上限。实际画面按游戏内容的宽高比缩放。
- 本地窗口可以缩放，按 F11 切换全屏，增强处理尺寸保持不变。

## 局域网串流

串流需要支持 H.264 硬件编码的驱动和 FFmpeg（VA-API 或 NVENC），以及 PipeWire、`pipewire-pulse`、`pw-loopback`、`pactl` 和 `parec`。

如果本地后端已在运行，先保存游戏并执行 `./target/release/galscaler shutdown`。将下面的 IP 换成电脑的局域网地址，在终端前台启动串流服务。`GALSCALER_TOKEN` 至少需要 32 个可见 ASCII 字符。

```sh
token=$(openssl rand -hex 32)
printf '%s\n' "$token"
GALSCALER_TOKEN="$token" ./target/release/galscaler serve --listen 192.168.1.20:9771
```

在同一局域网内，用浏览器打开 `http://192.168.1.20:9771`，或在 Android 客户端输入主机地址。主机安装 Avahi 并提供 `avahi-publish-service` 时，Android 客户端可以自动发现服务。连接后输入令牌，选择游戏开始串流。手机可轻点推进剧情、长按打开右键菜单、上下滑动滚动回看。

从手机启动的游戏在主机上无窗口运行。已开启增强的本地游戏也可串流，前提是它由启用了局域网服务的后端启动。HTTP 信令使用明文传输，服务应只对信任的局域网开放。

Android 客户端支持 Android 8.0（API 26）及以上。准备 JDK、Android SDK 和 NDK 后，从 `tools/galscaler/mobile` 构建。APK 位于 `dist/android/`。

```sh
go run github.com/egoist/mygo/cmd/mygo@v0.10.0 build -platform android
```

## 数据位置

引擎数据默认放在 `$XDG_DATA_HOME/galscaler`，未设置时使用 `~/.local/share/galscaler`。

| 路径 | 内容 |
| --- | --- |
| `library.json` | 游戏条目、全局设置和单游戏覆盖 |
| `prefixes/shared/`、`prefixes/<id>/` | 共享和独立的 Wine 环境，包含游戏写入该环境的存档 |
| `covers/` | 下载的封面 |
| `sessions.json`、`logs/`、`engine.log` | 会话状态、游戏日志和后台启动日志 |

桌面外观保存在 `$XDG_CONFIG_HOME/galscaler/appearance.json`，默认位于 `~/.config/`。默认 socket 是 `$XDG_RUNTIME_DIR/galscaler.sock`，未设置运行时目录时使用数据目录。可通过 `GALSCALER_DATA_DIR` 和 `GALSCALER_SOCKET` 指定其他实例。

维护与集成说明见 [引擎与接口](docs/engine.md)。

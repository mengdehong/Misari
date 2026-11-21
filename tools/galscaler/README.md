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

## 使用指南

- 导入游戏、选择 Proton 和 Wine 环境、设置画质或使用命令行。见 [游戏库与画质增强](docs/game-library.md)。
- 在桌面开启密码连接，用浏览器或 Android 客户端玩游戏。见 [局域网串流](docs/lan-streaming.md)。

## 数据位置

引擎数据默认放在 `$XDG_DATA_HOME/galscaler`，未设置时使用 `~/.local/share/galscaler`。

| 路径 | 内容 |
| --- | --- |
| `library.json` | 游戏条目、全局设置和单游戏覆盖 |
| `prefixes/shared/`、`prefixes/<id>/` | 共享和独立的 Wine 环境，包含游戏写入该环境的存档 |
| `covers/` | 下载的封面 |
| `lan.json` | 局域网连接开关与密码 |
| `sessions.json`、`logs/`、`engine.log` | 会话状态、游戏日志和后台启动日志 |

桌面外观保存在 `$XDG_CONFIG_HOME/galscaler/appearance.json`，默认位于 `~/.config/`。默认 socket 是 `$XDG_RUNTIME_DIR/galscaler.sock`，未设置运行时目录时使用数据目录。可通过 `GALSCALER_DATA_DIR` 和 `GALSCALER_SOCKET` 指定其他实例。

维护与集成说明见 [引擎与接口](docs/engine.md)。

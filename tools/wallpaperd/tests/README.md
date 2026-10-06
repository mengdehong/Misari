# wallpaperd 测试

统一入口：`python3 tools/wallpaperd/tests/check.py [组]`，默认 `quick`。入口负责依赖检查、私有环境及失败退出。

| 组 | 检查内容 |
| --- | --- |
| `quick` | 格式化、Clippy、Rust 单元测试 |
| `nested` | 非 Web 的完整嵌套集成套件 |
| `properties`、`mouse`、`library`、`audio` | 属性事务、鼠标遮挡、素材库轮播、自动静音 |
| `pixels`、`transitions`、`sync` | 场景像素、几何转场、跨屏时间轴 |
| `spectrum`、`sounds`、`multioutput` | 频谱、场景音效、双输出隔离 |
| `thumbnails` | 视频/着色器预览、损坏输入、缓存复用与更新 |
| `web`、`web-integration`、`web-audio` | CEF 生命周期、worker 恢复、音频控制与释放 |

嵌套套件默认使用 `niri/target/release/niri`，可用 `WALLPAPERD_TEST_NIRI` 覆盖。图形检查需要 Weston、EGL/GLES；音频检查使用私有 PipeWire/Pulse，双输出另需 labwc。各组会列出缺少的程序。Web 自动启用 `web` feature，需安装 CEF。

Web 默认使用 headless mmap；`WALLPAPERD_WEB_GPU=1` 使用硬件 EGL，`WALLPAPERD_WEB_DIRECT=1` 启用实验直接提交。GPU 检查需要嵌套 niri 或真实桌面，不能使用无 seat 的 headless Weston。

`nested.rs` 管理共享运行环境，`nested/` 按 compositor、state、playback、web 组织用例；`support/` 提供音频服务器、场景与 Weston 夹具，`fixtures/` 存放属性定义。

本轮删除独立 MPRIS 播放器模拟、重复的视频时序/切换和 Rust 鼠标 fit/parallax 组合，以及对应夹具、旧 X11 分支和兼容分组。缩略图保留视频与着色器两个代表路径，不再重复 GIF/作者预览/WE 视频组合。视频时序和渲染算法仍由 `src/content` 与 `we-scene` 检查；MPRIS 的 D-Bus 端到端覆盖已移除。

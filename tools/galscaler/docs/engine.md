# GalScaler 引擎与接口

## 进程与渲染

`galscaler` CLI 和 `ui/` 桌面客户端通过 Unix socket 连接 `galscalerd`。后端保存游戏库，为每次启动创建 systemd 用户 scope，并通过 `sessions.json` 找回重启后仍在运行的会话。

增强由独立渲染进程执行。Proton 游戏通过内部 Xwayland 接入 Smithay 子合成器，GLES 和 libplacebo 在 GPU 上完成合成与缩放。本地会话呈现为 Wayland 窗口，远程启动的会话使用离屏渲染。串流通过 WebRTC 传输 H.264 视频和 Opus 音频，触控输入走数据通道。

| 位置 | 职责 |
| --- | --- |
| `src/daemon.rs`、`src/ipc.rs`、`src/engine.rs` | 后端入口、Unix socket、游戏库操作与会话生命周期 |
| `src/model.rs`、`src/platform.rs` | 配置继承与持久化、Proton 发现及游戏启动 |
| `src/scan.rs`、`src/cover.rs` | 本地游戏扫描、VNDB 封面查找与下载 |
| `src/native.rs`、`src/render.rs`、`src/gpu.c` | 子合成器、输入映射、画面增强 |
| `src/web.rs`、`src/stream.rs`、`src/stream.c` | HTTP 信令、WebRTC、音视频采集与编码 |
| `ui/`、`mobile/` | Go 桌面界面、Android 客户端和共用的浏览器播放器 |

## Unix socket

每次连接发送一条以换行结尾的 JSON 请求，并读取一条响应。单条消息上限为 1 MiB。操作由 `op` 字段指定。完整请求类型见 [`src/api.rs`](../src/api.rs)。

```sh
printf '%s\n' '{"op":"status"}' | galscaler request
```

直接使用 socket 时，成功响应为 `{"ok":true,"result":...}`，失败响应为 `{"ok":false,"error":"..."}`。`galscaler request` 成功时只输出 `result`。

- `list` 返回应用全局设置后的游戏条目。`library` 返回全局设置和保存的游戏覆盖项。`set_settings` 替换全局设置。
- `launch` 返回会话。客户端可通过 `status` 查询会话状态，用 `log` 读取最近一次启动日志。`launch` 和 `doctor` 可以传入包含绝对 Wayland socket 路径的 `desktop`。
- `status` 中的增强会话可以提供实际输入尺寸 `metrics.source`、增强尺寸 `metrics.enhanced` 和显示尺寸 `metrics.display`。

## 局域网接口

开启监听后，`GET /` 提供浏览器播放器。控制和信令接口使用 `Authorization: Bearer <token>`。

| 接口 | 用途 |
| --- | --- |
| `POST /api/v1/rpc`、`GET /api/v1/ws` | 发送 JSON 请求。允许 `list`、`status`、`log`、`stop` 和 `launch_stream`。 |
| `POST /api/v1/stream/{id}` | 为指定游戏会话交换 WebRTC offer 和 answer。 |

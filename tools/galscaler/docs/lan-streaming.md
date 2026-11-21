# GalScaler 局域网串流

在电脑上运行游戏，用同一局域网内的浏览器或 Android 客户端播放画面和声音。电脑需要支持 H.264 硬件编码的驱动和 FFmpeg（VA-API 或 NVENC），以及 PipeWire、`pipewire-pulse`、`pw-loopback`、`pactl` 和 `parec`。

## 开启连接

1. 在 GalScaler 桌面端打开「全局设置」，进入「局域网连接」。
2. 打开「允许局域网连接」。首次开启时设置 6–64 个字符的连接密码，点击「保存并开启」。
3. 点击「连接地址」一栏中的网址即可复制。默认使用电脑的局域网 IPv4 地址和 `9771` 端口。

用浏览器打开连接地址，或在 Android 客户端选择「手动连接」并输入地址，再输入电脑上设置的密码。电脑安装 Avahi 并提供 `avahi-publish-service` 时，Android 客户端也可以自动发现设备。Android 客户端支持记住密码，并可在「连接详情」里打开「启动时自动连接」。

HTTP 信令使用明文传输，请在可信的局域网内使用。关闭局域网连接或修改密码会断开当前串流。

## 播放游戏

连接后，在游戏库点击游戏的播放按钮。远程启动的游戏会在电脑上离屏运行。已经在电脑上启动的游戏，需要在启动时同时开启画质增强和局域网服务，才能接入串流。

- 轻点选择或推进剧情，长按打开右键菜单。
- 上下滑动滚动回看，横向滑动模拟鼠标拖拽。
- 在游戏详情里可以调整增强算法和帧率，保存后在下次启动时生效。

## 指定监听地址

需要指定 IP 或端口时，可以在 `tools/galscaler` 完成 release 构建后前台启动。若后端已在运行，先在游戏内存档，再用 `./target/release/galscaler shutdown` 结束现有会话和后端。将示例中的 IP 换成电脑的局域网地址。

```sh
./target/release/galscaler serve --listen 192.168.1.20:9771
```

前台启动默认使用已保存的连接密码，也可以用 `GALSCALER_PASSWORD` 指定本次启动的密码。

## 构建 Android 客户端

客户端支持 Android 8.0（API 26）及以上。准备 Go、JDK、Android SDK 和 NDK 后，在 `tools/galscaler/mobile` 运行以下命令。APK 位于 `dist/android/`。

```sh
go run github.com/egoist/mygo/cmd/mygo@v0.10.0 build -platform android
```

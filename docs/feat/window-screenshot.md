# 指定窗口截图

先查询窗口 ID，再截图；将示例中的 `42` 换成目标 ID。

```bash
niri msg --json windows --app-id '^firefox$' --title 'GitHub'
niri msg action screenshot-window --id 42 --path /tmp/window-42.png --silent --wait
```

- `--silent`：不修改剪贴板、不发送桌面通知；仍报告错误并发送 IPC 截图事件。
- `--wait`：等待编码、写盘及剪贴板更新（若启用）完成，输出保存路径；失败以非零状态退出。加 `--json` 输出 `{"path":"/tmp/window-42.png"}`。

两者独立、默认关闭，省略时保持原有行为。KDL 可用 `screenshot-window silent=true;`，`wait` 仅用于 CLI／IPC。

截图不改变焦点或工作区；后台窗口使用最近提交的画面。省略 `--id` 时截取聚焦窗口，指定 ID 不存在时不回退到其他窗口。

`--path` 的父目录须已存在，已有文件会被覆盖；省略时沿用 `screenshot-path`。带 `--wait` 请求写盘但未配置保存路径时返回错误。`--write-to-disk=false` 禁用写盘，等待结果为 `{"path":null}`，普通输出为空；再加 `--silent` 则不会产生文件或剪贴板内容。

IPC 沿用 `Action.ScreenshotWindow`，新增 `silent`、`wait` 两个布尔字段，默认均为 `false`。CLI 将相对路径转为绝对路径，直接调用 IPC 必须传绝对路径。

`wait=true` 通过当前连接返回 `{"Ok":{"ScreenshotSaved":{"path":"/tmp/window-42.png"}}}` 或 `{"Err":"原因"}`，无需订阅事件流；`wait=false` 仍返回 `Handled`。等待不阻塞桌面事件循环，也不等待桌面通知送达。

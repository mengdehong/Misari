# 窗口跟随

切换工作区或显示器时，将跟随窗口带到当前聚焦工作区。浮动／平铺状态保持；目标已有活动窗口时不抢焦点，空工作区沿用现有聚焦规则。

## 按窗口规则自动开启

在 `window-rule` 中设置 `open-follow-mode`：

```kdl
window-rule {
    match app-id="^mpv$"
    open-follow-mode "if-invisible"
}
```

| 模式 | 行为 |
| --- | --- |
| `off` | 关闭跟随，默认值。 |
| `always` | 切换聚焦工作区时，移动到当前聚焦工作区。 |
| `if-invisible` | 原工作区仍在某块显示器上显示时留在原处；原工作区不再显示时，移动到当前聚焦工作区。 |

“不再显示”指工作区不是其显示器的活动工作区，不判断窗口遮挡、全屏覆盖或水平滚动位置。单显示器下，两个跟随模式效果相同。

后面匹配的规则覆盖前面的模式。规则只设置新窗口的初始模式，修改配置不改变已有窗口。

## 手动开关

操作当前聚焦窗口：

```kdl
binds {
    Mod+F9 repeat=false { toggle-window-follow; }
}
```

`toggle-window-follow` 在关闭时开启 `always`，已开启时关闭。`set-window-follow` 明确设置模式；手工设置不因规则重算而重置。

设置模式后，下次工作区聚焦或激活变化时跟随。手动移动、暂存仍保留模式；要让窗口留在暂存区，先关闭跟随。

## 命令行与 IPC

省略 `--id` 时操作当前聚焦窗口，`42` 替换为实际窗口 ID。无焦点窗口或 ID 不存在时不做操作：

```sh
niri msg action toggle-window-follow
niri msg action set-window-follow if-invisible --id 42
niri msg action set-window-follow off --id 42
niri msg --json windows
```

IPC 设置示例：

```json
{"Action":{"SetWindowFollow":{"id":42,"mode":"IfInvisible"}}}
```

窗口查询的 `follow_mode` 为 `Off`、`Always` 或 `IfInvisible`，缺失时默认 `Off`；变化通过 `WindowOpenedOrChanged` 事件发布。

## 切换与窗口生命周期

工作区手势确定目标后跟随；拖动窗口期间暂缓，结束后按最终工作区处理。窗口仍属于一个工作区，参与切换动画。

所有显示器断开时保留模式，重连后恢复处理。窗口关闭或解除映射后释放状态，新窗口按规则初始化。

## 从外部跟随脚本迁移

更新 Niri 后重新进入会话，配置跟随规则，并移除外部跟随脚本的启动项、停止其进程。niriusd 的其他功能可继续使用。

# 窗口跟随

切换工作区或显示器时，将开启跟随的窗口带到当前聚焦工作区。支持浮动和平铺窗口，移动保留原有布局状态；目标工作区已有活动窗口时不抢焦点，空工作区遵循现有聚焦规则。

## 按窗口规则自动开启

在 `config.kdl` 的 `window-rule` 中设置 `open-follow-mode`。如果配置已拆分并包含 `rule.kdl`，可直接加在该文件中：

```kdl
window-rule {
    match app-id="^mpv$"
    open-floating true
    open-follow-mode "if-invisible"
}
```

匹配的窗口打开时自动启用跟随，无需快捷键。`open-floating` 单独决定是否浮动；平铺窗口同样可以设置跟随模式。

| 模式 | 行为 |
| --- | --- |
| `off` | 关闭跟随。 |
| `always` | 切换聚焦工作区时，移动到当前聚焦工作区。 |
| `if-invisible` | 原工作区仍在某块显示器上显示时留在原处；原工作区不再显示时，移动到当前聚焦工作区。 |

“不再显示”指工作区不是其显示器的活动工作区，不判断窗口遮挡、全屏覆盖或水平滚动位置。单显示器下，两个跟随模式效果相同。

未配置时默认 `off`。规则沿用 Niri 的匹配和覆盖顺序，后面匹配的规则可用 `open-follow-mode "off"` 覆盖前面的模式。与 `open-floating` 一样，此规则设置打开时的初始状态；修改规则只影响之后打开的窗口，已有窗口保留当前模式。

## 手动开关

需要临时切换当前聚焦窗口时，可选用一个快捷键：

```kdl
binds {
    Mod+F9 repeat=false { toggle-window-follow; }
}
```

`toggle-window-follow` 在关闭时开启始终跟随；任何跟随模式已开启时将其关闭。`set-window-follow` 明确设置模式，重复执行保持相同结果。手动设置不会因规则重新计算而重置。

模式设置只修改窗口状态，下一次工作区聚焦或激活变化时执行跟随。手动移动或暂存窗口不会立即将其搬回，跟随模式继续保留；需要让窗口长期留在暂存区时先关闭跟随。

## 命令行与 IPC

命令行使用同一动作，省略 `--id` 时操作当前聚焦窗口：

```sh
niri msg action toggle-window-follow
niri msg action set-window-follow if-invisible --id 42
niri msg action set-window-follow off --id 42
niri msg --json windows
```

将 `42` 替换为窗口查询返回的 ID。没有聚焦窗口或指定 ID 不存在时不做操作。

窗口查询和事件流的窗口对象包含 `follow_mode`，取值为 `Off`、`Always`、`IfInvisible`。模式变化通过现有 `WindowOpenedOrChanged` 事件发布，读取旧窗口对象时缺失字段默认表示 `Off`。

例如，通过 IPC 设置指定窗口的按需跟随模式：

```json
{"Action":{"SetWindowFollow":{"id":42,"mode":"IfInvisible"}}}
```

## 切换与窗口生命周期

跟随由 Niri 直接处理。工作区手势在确定目标后执行跟随；交互拖动窗口期间暂缓，拖动结束后按最终工作区状态处理。

所有显示器断开时保留模式，重新连接后根据恢复的工作区状态处理。窗口关闭或解除映射后，该窗口的跟随状态随之释放，新映射的窗口按窗口规则初始化模式。

窗口跟随通过移动窗口实现。窗口仍属于一个工作区，会参与工作区切换动画。

## 从外部跟随脚本迁移

更新为支持此功能的 Niri 后，需重新进入会话使新的合成器生效。在窗口规则中设置跟随模式，并移除外部跟随脚本的启动项、停止其残留进程，避免重复搬动窗口。niriusd 的其他功能可以继续使用。

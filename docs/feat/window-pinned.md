# 窗口置顶

`pinned` 让浮动窗口保持在普通窗口和全屏应用上方，不抢焦点；多个置顶窗口沿用浮动窗口排序。

## 快捷键

操作当前聚焦窗口：

```kdl
binds {
    Mod+MouseMiddle repeat=false { toggle-window-pinned when-tiled="float"; }
    Mod+P repeat=false { toggle-window-pinned; }
    Mod+Shift+P repeat=false { set-window-pinned "auto"; }
}
```

Mod+中键在“浮动并置顶”和“取消置顶并平铺”之间切换，覆盖默认的工作区视图拖动操作。

`toggle-window-pinned` 的 `when-tiled` 默认 `ignore`：

| 值 | 平铺窗口（包括全屏、最大化） | 浮动窗口 |
| --- | --- | --- |
| `ignore` | 不做操作。 | 切换置顶，保持浮动。 |
| `remember` | 切换这个窗口的置顶设置，转浮动后生效。 | 切换置顶，保持浮动。 |
| `float` | 浮动并置顶。 | 未置顶时开启置顶；已置顶时取消置顶并转平铺。 |

`set-window-pinned "on"/"off"` 明确开启或取消置顶，平铺窗口按 `when-tiled` 处理。显式 `off` 保持布局；`auto` 清除手工设置、恢复遵循规则，所有布局均可执行。

## 窗口规则

在 `window-rule` 中设置 `pinned`：

```kdl
window-rule {
    match app-id="firefox$" title="^Picture-in-Picture$"
    pinned true
}
```

默认 `false`，后面匹配的规则覆盖前面的值；配置重载或匹配条件变化时更新。仅浮动时生效，打开时是否浮动由 `open-floating` 决定。

手工设置优先于规则，重载配置不覆盖；`auto` 恢复遵循规则。窗口关闭或解除映射后释放手工设置。

## CLI 与 IPC

省略 `--id` 时操作聚焦窗口；无焦点窗口或 ID 不存在时不做操作。`42` 替换为实际窗口 ID：

```sh
niri msg action toggle-window-pinned --id 42 --when-tiled float
niri msg action set-window-pinned on --id 42 --when-tiled remember
niri msg action set-window-pinned off --id 42
niri msg action set-window-pinned auto --id 42
niri msg --json windows
```

IPC 示例：

```json
{"Action":{"ToggleWindowPinned":{"id":42,"when_tiled":"Float"}}}
```

`SetWindowPinned` 的 `mode` 为 `On`、`Off` 或 `Auto`；两个动作的 `when_tiled` 为 `Ignore`、`Remember` 或 `Float`，省略时为 `Ignore`。

查询和 `WindowOpenedOrChanged` 事件中的 `pinned` 表示窗口自身的设置，不包含继承层级。平铺窗口也可查询，缺失时表示未知。

## 层级与生命周期

浮动子窗口继承父窗口层级，保持在父窗口上方，不改变自身的 `pinned`；popup 随所属窗口绘制。

浮动／平铺切换、全屏切换、工作区和显示器迁移均保留设置。平铺时置顶不生效，转浮动后恢复；拖拽预览、layer-shell、锁屏和截图界面保留原有优先级。

## 用法示例：跨工作区画中画

组合浮动、置顶和[窗口跟随](window-follow.md)，让画中画窗口在切换工作区后继续显示：

```kdl
window-rule {
    match app-id="firefox$" title="^Picture-in-Picture$"
    open-floating true
    pinned true
    open-follow-mode "if-invisible"
}
```

置顶控制层级；跟随负责工作区迁移，规则只设置新窗口的跟随模式。

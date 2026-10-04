# 窗口置顶

开启 `pinned` 后，浮动窗口保持在普通浮动窗口和平铺窗口上方，包括聚焦的全屏应用。开启置顶不抢焦点；多个置顶窗口沿用组内的浮动窗口排序。

## 快捷键

鼠标和键盘绑定都操作当前聚焦窗口，使用前先聚焦目标窗口：

```kdl
binds {
    Mod+MouseMiddle repeat=false { toggle-window-pinned when-tiled="float"; }
    Mod+P repeat=false { toggle-window-pinned; }
    Mod+Shift+P repeat=false { set-window-pinned "auto"; }
}
```

Mod+中键在“浮动并置顶”和“取消置顶并平铺”之间切换，覆盖默认的工作区视图拖动操作。

`toggle-window-pinned` 的 `when-tiled` 设置布局行为，默认 `ignore`：

| 值 | 平铺窗口（包括全屏、最大化） | 浮动窗口 |
| --- | --- | --- |
| `ignore` | 不做操作。 | 切换置顶，保持浮动。 |
| `remember` | 切换这个窗口的置顶设置，转浮动后生效。 | 切换置顶，保持浮动。 |
| `float` | 浮动并置顶。 | 未置顶时开启置顶；已置顶时取消置顶并转平铺。 |

`set-window-pinned "on"/"off"` 明确开启或取消置顶，平铺窗口按 `when-tiled` 处理。显式 `off` 保持布局；`auto` 清除手工设置、恢复遵循规则，所有布局均可执行。

## 窗口规则

按 app-id/title 设置同类窗口的默认行为：

```kdl
window-rule {
    match app-id="firefox$" title="^Picture-in-Picture$"
    open-floating true
    pinned true
    open-follow-mode "if-invisible"
}
```

`pinned` 默认 `false`，后面匹配的规则覆盖前面的值。配置重载、标题或匹配条件变化会更新规则。`pinned true` 只设置置顶，窗口打开时是否浮动由 `open-floating` 决定。

手工设置只作用于当前窗口，优先于规则，配置重载和规则重算不会覆盖它。执行 `set-window-pinned "auto"` 恢复遵循规则；窗口关闭或解除映射后，手工设置释放，新窗口按规则初始化。

置顶窗口仍属于一个工作区。需要跨工作区继续显示时，搭配[窗口跟随](window-follow.md)的 `open-follow-mode "if-invisible"` 或 `"always"`；跟随规则只设置新窗口的初始模式。

## CLI 与 IPC

省略 `--id` 时操作聚焦窗口；没有聚焦窗口或 ID 不存在时不做操作。将 `42` 换成窗口查询返回的 ID：

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

窗口查询和 `WindowOpenedOrChanged` 事件的 `pinned` 字段表示窗口自己的置顶设置，合并规则和手工设置，不包含子窗口继承的层级。平铺窗口也可查询；旧版本缺失该字段时表示未知。

## 层级与生命周期

浮动子窗口继承父窗口的置顶层级，保持在父窗口上方；继承不修改子窗口自身的 `pinned`。菜单等 popup 随所属窗口绘制。

原生 `toggle-window-floating`、自身全屏与解除全屏、工作区和显示器迁移均保留置顶设置。平铺时置顶暂不生效，再转浮动时恢复。拖拽预览、layer-shell 图层、锁屏和截图界面保留原有优先级。

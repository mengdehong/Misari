# 屏幕边缘滚动

Niri 通过 `edge-scroll` 匹配屏幕边缘的滚动输入，执行已有动作。

## 配置

在 `config.kdl` 或其包含的文件中添加区域。例如，顶部滚动切换工作区：

```kdl
edge-scroll "top" width=4 {
    binds {
        WheelScrollUp { focus-workspace-up; }
        WheelScrollDown { focus-workspace-down; }
        TouchpadScrollUp { focus-workspace-up; }
        TouchpadScrollDown { focus-workspace-down; }
    }
}
```

| 配置 | 含义 |
| --- | --- |
| 边缘 | `top`、`bottom`、`left`、`right`，按显示器可见方向判断。 |
| `width` | 向内的宽度，单位为逻辑像素；默认 4，范围 `(0, 65535]`。 |
| `output` | 可选连接器名称，省略时作用于所有显示器；用 `niri msg outputs` 查询。 |
| `binds` | `WheelScroll` 或 `TouchpadScroll` 的 `Up/Down/Left/Right`，支持修饰键、普通动作和 `cooldown-ms`。 |

区域可重复声明，按顺序取首个匹配；修饰键精确匹配。同来源、同修饰键的普通滚动绑定优先。

锁屏、概览、截图等合成器交互期间，以及指针被抓取或锁定时停用。命中后整个滚动帧由区域接管；滚轮逐帧判断，触摸板固定首次匹配，直到手势结束或被取消。重载配置会取消已接管的手势。

## 用法示例：音量与亮度

Niri 用 `spawn` 启动命令；调节音量、亮度及显示反馈由 Noctalia 提供。

以下示例替换前面的区域：顶部普通滚动调音量，Ctrl + 滚动调亮度。需运行 Noctalia 并启用对应后端，将所有 `eDP-1` 替换为实际连接器名称：

```kdl
edge-scroll "top" width=4 output="eDP-1" {
    binds {
        WheelScrollUp cooldown-ms=50 { spawn "noctalia" "msg" "volume-up" "2%"; }
        WheelScrollDown cooldown-ms=50 { spawn "noctalia" "msg" "volume-down" "2%"; }
        TouchpadScrollUp cooldown-ms=50 { spawn "noctalia" "msg" "volume-up" "2%"; }
        TouchpadScrollDown cooldown-ms=50 { spawn "noctalia" "msg" "volume-down" "2%"; }
        Ctrl+WheelScrollUp cooldown-ms=50 { spawn "noctalia" "msg" "brightness-up" "eDP-1" "2%"; }
        Ctrl+WheelScrollDown cooldown-ms=50 { spawn "noctalia" "msg" "brightness-down" "eDP-1" "2%"; }
        Ctrl+TouchpadScrollUp cooldown-ms=50 { spawn "noctalia" "msg" "brightness-up" "eDP-1" "2%"; }
        Ctrl+TouchpadScrollDown cooldown-ms=50 { spawn "noctalia" "msg" "brightness-down" "eDP-1" "2%"; }
    }
}
```

`output` 筛选区域，不会替换命令参数；多屏亮度调节需为各显示器分别配置。

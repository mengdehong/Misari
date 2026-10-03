# 窗口暂存与召回

按条件将应用窗口带到当前工作区并聚焦；没有匹配窗口时，可按配置启动应用并完成召回。

## 暂存窗口

通过命名工作区和现有移动、聚焦动作即可暂存窗口并进入暂存区操作：

```kdl
workspace "stash"
binds {
    Mod+0 { focus-workspace "stash"; }
    Mod+Shift+0 repeat=false { move-window-to-workspace "stash" focus=false; }
}
```

`Mod+Shift+0` 将当前窗口移到 `stash`，焦点留在当前工作区；`Mod+0` 进入暂存区。
名称提供稳定引用，工作区位置仍按现有规则变化。`stash` 是普通命名工作区，不会隐藏；召回会查询所有工作区。

## 召回窗口

聚焦是切换到窗口所在工作区；召回是将窗口移到当前聚焦的工作区并聚焦。
目标窗口已在当前工作区时只聚焦，不重复搬移。
召回保持窗口已有的浮动／平铺状态，按目标工作区规则安置，不恢复历史排列位置。

在 `config.kdl` 中绑定 `recall-window`。属性表示匹配条件，可选字符串参数表示无匹配时的启动命令：

```kdl
binds {
    Mod+B repeat=false { recall-window app-id="^chrome$"; }
    Mod+T repeat=false { recall-window "chrome" app-id="^chrome$"; }
}
```

命令行使用同一动作：

```sh
niri msg action recall-window --app-id '^chrome$'
niri msg action recall-window --app-id '^chrome$' -- chrome
```

首个字符串参数是程序，后续字符串是程序参数，不隐式经过 shell；省略全部字符串参数时只召回已有窗口。示例中的程序名和 app-id 需按实际应用修改，app-id 可用 `niri msg windows` 查看。

召回支持[窗口筛选条件](window-selection.md#筛选条件)，多个条件取交集，至少需要一个有效条件。空条件和非法正则会报错。带启动命令时仅允许 `app-id`、`title` 条件，不能附带窗口 ID、工作区或状态条件。

## 窗口选择

优先选择当前聚焦的匹配窗口，否则选择最近使用的匹配窗口；有焦点时间的优先于无记录的，时间相同或均无记录时取 ID 较小者。焦点时间受 `recent-windows.debounce-ms` 影响，与[按条件聚焦](window-selection.md#聚焦)一致。

没有匹配窗口且未配置启动命令时不做操作。重复执行不会在匹配窗口间循环，也不会自动切换暂存／召回。

## 启动后召回

没有匹配窗口且配置了启动命令时，记录触发时的目标工作区 ID，启动应用并等待窗口出现。窗口显示或 app-id／标题更新后满足条件时，完成一次召回。

等待期间，相同条件与命令的重复请求不会重复启动，只将目标工作区更新为最近一次触发的位置，不延长等待时间。完成时若用户已切到其他工作区，只将窗口安置到目标工作区，不强行切回。

优先通过应用回传的启动激活 token 关联窗口；未回传时，仅匹配请求后新出现的窗口。这种后备匹配不保证窗口来自该启动进程，因此条件应尽量具体。

等待时间为 10 秒。启动失败、超时或目标工作区消失时结束等待，不终止应用；超时后应用仍可能正常打开窗口。

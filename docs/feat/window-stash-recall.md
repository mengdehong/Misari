# 窗口召回

按条件将应用窗口带到当前工作区并聚焦；没有匹配窗口时，可按配置启动应用并完成召回。

## 配置与命令

`recall-window` 的属性是匹配条件，可选字符串参数是无匹配时的启动命令：

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

首个字符串是程序，后续字符串是参数，不经过 shell；省略命令时只召回已有窗口。程序名和 app-id 按实际应用修改，app-id 可用 `niri msg windows` 查看。

支持[窗口筛选条件](window-selection.md#筛选条件)，至少提供一个有效条件；带启动命令时仅允许 `app-id`、`title`。

## 召回行为

选择规则与[按条件聚焦](window-selection.md#聚焦)一致。召回将窗口带到当前工作区；目标已在当前工作区时只聚焦。浮动／平铺状态保持，按目标工作区规则安置，不恢复历史排列。

没有匹配窗口且未配置命令时不做操作。重复执行不会循环选择窗口，也不会切换暂存／召回。

## 启动后召回

无匹配且配置了命令时启动应用，等待窗口出现或 app-id／标题更新满足条件，再召回到触发时的工作区。

等待期间，相同条件与命令的请求只更新目标工作区，不重复启动、不延长等待。用户已切到其他工作区时，完成召回不强行切回。

优先通过启动激活 token 关联窗口；未回传时匹配请求后新出现的窗口，不能保证来自该进程，因此条件应尽量具体。

等待 10 秒。启动失败、超时或目标工作区消失时结束等待，不终止应用。

<a id="暂存窗口"></a>

## 用法示例：命名工作区暂存

用命名工作区配合现有移动、聚焦动作暂存窗口，再用 `recall-window` 取回：

```kdl
workspace "stash"
binds {
    Mod+0 { focus-workspace "stash"; }
    Mod+Shift+0 repeat=false { move-window-to-workspace "stash" focus=false; }
}
```

Mod+Shift+0 暂存当前窗口并留在原工作区；Mod+0 进入暂存区。`stash` 是普通命名工作区，不会隐藏；召回默认查找所有工作区。

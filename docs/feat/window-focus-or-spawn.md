# 聚焦或启动应用

`focus-or-spawn` 有匹配窗口时聚焦它，没有时启动指定程序。窗口位于其他工作区或输出时切到其所在地，窗口本身不移动。

## 使用

```kdl
binds {
    Mod+B { focus-or-spawn "firefox" app-id="^firefox$"; }
    Mod+G { focus-or-spawn "firefox" "https://github.com" app-id="^firefox$" title="GitHub"; }
}
```

```sh
niri msg action focus-or-spawn --app-id '^firefox$' -- firefox
```

只接受 `app-id`、`title`，至少提供一个，多个条件取交集。正则规则见[窗口筛选](window-selection.md#筛选条件)，实际 app-id 用 `niri msg windows` 查询。

启动命令必填：首个字符串是程序，后续是参数，不经过 shell。程序名非空，参数可为空，字符串不得含 NUL。空条件或非法正则报错，不支持 `allow-when-locked`。

窗口选择规则与[按条件聚焦](window-selection.md#聚焦)一致。

## 启动去重

等待窗口期间，相同条件与命令不重复启动、不延长等待。绑定、CLI 和 IPC 共用去重，不必配置 `repeat=false`；不同条件或命令独立处理。

窗口出现或 app-id／标题更新满足条件后结束等待。启动失败或超时 10 秒后允许重试，不终止已启动的应用。

新窗口沿用 `spawn` 的窗口规则、放置与激活策略；匹配后不额外搬移或聚焦。用户切到其他位置后，应用仍可能通过有效激活 token 获取焦点。

将窗口带到当前工作区用[召回](window-stash-recall.md)，两种动作独立去重。

## IPC

```json
{"Action":{"FocusOrSpawn":{"filter":{"app_id":"^firefox$"},"command":["firefox","--new-window"]}}}
```

校验通过后返回 `Handled`，不代表启动成功或窗口已出现；启动错误写入 niri 日志。

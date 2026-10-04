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
niri msg action focus-or-spawn --app-id '^firefox$' -- firefox --new-window
```

只允许 `app-id`、`title` 条件，至少提供一个；多个条件取交集。正则规则与[窗口筛选](window-selection.md#筛选条件)相同。实际应用标识可用 `niri msg windows` 查看。

启动命令必填。第一个字符串是程序名，后续字符串是参数，不隐式经过 shell。程序名不能为空，参数允许空字符串，但不能包含 NUL。空条件、非法正则、窗口 ID、工作区及窗口状态条件均会报错。该动作不支持 `allow-when-locked`。

当前焦点窗口匹配时保持焦点，否则选择最近使用的匹配窗口；没有焦点记录或记录相同时，选择 ID 较小者。规则与[按条件聚焦](window-selection.md#聚焦)一致，重复触发不会循环切换窗口。

## 启动去重

相同匹配条件与命令正在等待窗口出现时，重复请求不会再次启动，也不延长等待时间。去重由动作统一处理，按键绑定、CLI 和 IPC 共用，不需要在绑定上填写 `repeat=false`。

窗口显示或 app-id／标题更新后满足条件时，结束等待。启动失败或等待超过 10 秒也会清理记录，后续请求可以重新启动；清理不会终止应用。不同条件或不同命令是独立请求。

启动后的窗口沿用普通 `spawn` 的窗口规则、放置与激活策略。等待记录只用于去重，不把窗口移回触发时的工作区，也不在匹配完成时额外聚焦。用户启动后切到其他位置，应用仍可能通过有效激活 token 稍后取得焦点。

需要将窗口带到当前工作区时使用[召回](window-stash-recall.md)。召回请求与 focus-or-spawn 请求独立去重。

## IPC

```json
{"Action":{"FocusOrSpawn":{"filter":{"app_id":"^firefox$"},"command":["firefox","--new-window"]}}}
```

筛选与是否启动的判断在合成器事件循环内完成。请求通过校验后返回 `Handled`，不表示程序已经成功启动或窗口已经出现；启动错误写入 niri 日志。

## 开发验证

`just dev` 或 `just dev-tty` 加载 [dev.kdl](../../configs/dev.kdl)。按 F5 启动测试终端，再按 F5 聚焦已有终端。把终端移到其他工作区后按 F5，应切到该工作区；保持按键时应复用启动中请求或保持已有窗口焦点。关闭测试终端后按 F5，应重新启动。

开发实例内使用仓库的 `niri/target/debug/niri` 执行 CLI，确保命令版本与 `NIRI_SOCKET` 对应开发实例。

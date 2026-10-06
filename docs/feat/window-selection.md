# 窗口查询与聚焦

按条件查找并聚焦窗口。查询、聚焦与配置绑定共用同一组条件。

## 筛选条件

多个条件取交集，未提供的条件不作限制。

| CLI 参数 | KDL 属性 | 含义 |
| --- | --- | --- |
| `--id 42` | `id=42` | 窗口 ID |
| `--app-id '^firefox$'` | `app-id="^firefox$"` | 应用标识正则 |
| `--title 'GitHub'` | `title="GitHub"` | 标题正则 |
| `--workspace-id 12` | `workspace-id=12` | 工作区 ID，不是显示编号 |
| `--current-workspace` | `current-workspace=true` | 执行时拥有焦点的工作区 |
| `--floating` / `--floating=false` | `floating=true` / `floating=false` | 浮动／平铺；省略时不限 |
| `--urgent` / `--urgent=false` | `urgent=true` / `urgent=false` | 紧急／非紧急；省略时不限 |

- 正则默认部分匹配，完整匹配用 `^…$`；非法正则在查询和配置加载时报错。
- 缺少 app-id 或标题的窗口不满足对应条件，`.*` 也不例外。
- `--floating` 与 `--urgent` 取布尔值时必须写成 `--floating=false`，中间不留空格。
- `current-workspace` 仅在 `true` 时生效；`false` 等同于未提供，不能作为唯一的聚焦条件。
- 工作区 ID 可用 `niri msg workspaces` 查看。

## 查询

```bash
niri msg windows
niri msg --json windows
niri msg windows --app-id '^firefox$' --title 'GitHub'
niri msg windows --current-workspace --floating=false
niri msg windows --group-by workspace
```

无条件时列出全部应用窗口，不含面板等 layer-shell surface。JSON 返回扁平窗口数组，`workspace_id` 关联 `niri msg --json workspaces` 的结果。

`--group-by workspace` 仅用于文本输出，不能与 `--json` 同用。窗口按工作区分组，组间按输出名、工作区编号排序，组内按窗口 ID 排序：

```text
DP-1 / Workspace 1 "web" (ID 12)
  42  "firefox"  "GitHub" [focused]
  57  "foot"  "~/misari"
No workspace
  63  "(unset)"  "(unset)"
```

工作区已消失时仍显示窗口及工作区 ID；没有工作区的窗口列入 `No workspace`。

## 聚焦

```bash
niri msg action focus-window --id 42
niri msg action focus-window-matching --app-id '^firefox$'
niri msg action focus-window-matching --title 'GitHub' --current-workspace
```

`focus-window-matching` 至少需要一个有效条件。当前焦点窗口匹配时保持焦点，否则选择最近使用的匹配窗口；有记录者优先，时间相同或均无记录时取 ID 较小者。焦点记录受 `recent-windows.debounce-ms` 影响。

目标在其他工作区或输出时切换过去，窗口不移动。无匹配时不做操作，重复执行不循环选择窗口。

没有匹配窗口时需要启动应用，使用 [focus-or-spawn](window-focus-or-spawn.md)。

在 `config.kdl` 中可直接绑定同一动作：

```kdl
binds {
    Mod+B { focus-window-matching app-id="^firefox$"; }
    Mod+G { focus-window-matching app-id="^firefox$" title="GitHub"; }
    Mod+T { focus-window-matching app-id="^foot$" current-workspace=true; }
}
```

配置加载时检查空条件和正则语法；该动作不支持 `allow-when-locked`。

## IPC

`WindowsMatching` 按条件查询，返回 `Response::Windows`；`FocusWindowMatching` 按条件聚焦：

```json
{"WindowsMatching":{"app_id":"^firefox$","floating":false}}
{"Action":{"FocusWindowMatching":{"filter":{"app_id":"^firefox$"}}}}
```

省略的条件不限，`current_workspace` 默认 `false`。聚焦返回 `Handled`，无匹配时也如此；需确认候选时先查询，再按 ID 聚焦。

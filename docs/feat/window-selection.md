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

不带条件时列出全部受管理的应用窗口，不含面板等 layer-shell surface。JSON 保持原有扁平窗口数组与字段，窗口通过 `workspace_id` 关联 `niri msg --json workspaces` 的结果。

`--group-by workspace` 仅用于文本输出，不能与 `--json` 同用。窗口按工作区分组，组间按输出名、工作区编号排序，组内按窗口 ID 排序：

```text
DP-1 / Workspace 1 "web" (ID 12)
  42  "firefox"  "GitHub" [focused]
  57  "foot"  "~/oh-my-niri"
No workspace
  63  "(unset)"  "(unset)"
```

窗口与工作区分两次查询。工作区在两次查询之间消失时，其窗口仍保留并标注工作区 ID；没有工作区的窗口列入 `No workspace`。

## 聚焦

```bash
niri msg action focus-window --id 42
niri msg action focus-window-matching --app-id '^firefox$'
niri msg action focus-window-matching --title 'GitHub' --current-workspace
```

`focus-window-matching` 至少需要一个有效条件。当前焦点窗口匹配时保持焦点，否则选择最近使用的匹配窗口，有焦点时间的优先于无记录的；时间相同或均无记录时取 ID 较小者。焦点时间复用 niri 的 MRU 记录，受 `recent-windows.debounce-ms` 影响。

目标在其他工作区或输出时，焦点转到目标所在地，窗口本身不移动。没有匹配窗口时不改变焦点，也不启动应用；重复执行不会在匹配窗口间循环。

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

原有 `Windows` 查询和按 ID 聚焦请求不变。带条件查询使用新增请求，结果仍为 `Response::Windows`；按条件聚焦使用新增动作：

```json
{"WindowsMatching":{"app_id":"^firefox$","floating":false}}
{"Action":{"FocusWindowMatching":{"filter":{"app_id":"^firefox$"}}}}
```

省略的条件不限，`current_workspace` 默认为 `false`。筛选和聚焦在合成器事件循环内执行。无匹配的聚焦仍返回 `Handled`，它不表示一定有窗口匹配。界面需要展示候选时，使用查询或现有事件流，再按 ID 聚焦用户选中的窗口。

## 开发验证

在仓库根目录运行 `just dev` 启动嵌套实例，或切到空闲 TTY 登录后运行 `just dev-tty`。两者都会构建新版并加载 [dev.kdl](../../configs/dev.kdl)，打开 `niri-dev`、`selection-A`、`selection-B` 三个终端。

| 按键 | 验证行为 |
| --- | --- |
| F6 | 按标题聚焦 A；A 在其他工作区时跳转过去 |
| F7 | 聚焦测试应用；当前已在 A／B 时保持焦点，否则选择最近使用的测试窗口 |
| F8 | 无匹配，保持焦点 |
| F9 | 返回 `niri-dev` 终端 |
| F10 | 退出开发实例 |

验证 CLI 时在开发实例的终端中使用仓库内的 `niri/target/debug/niri`，确保命令版本和 `NIRI_SOCKET` 都对应开发实例。

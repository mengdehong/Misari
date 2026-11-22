# 命名与版本规则

misari 是个人维护的基于 niri 开发的桌面项目。

## 命名与安装

- 项目与安装包名使用 `misari`，安装路径、命令、配置路径、IPC 与桌面环境标识均保留为 `niri`。
- 安装包提供 `niri`，与官方 `niri` 包冲突。

## Misari 版本

版本格式为 `上游版本.vX.Y`，tag 格式为 `misari-v<版本>`。上游版本采用 niri 的 Cargo 写法，例如 niri `26.04` 对应 `26.4.0`，`26.04.1` 对应 `26.4.1`。

`X` 为功能发布号，递增时将 `Y` 归零。`Y` 为修复号。每次切换上游基线，从 `v1.0` 开始。

| 变化 | Tag 示例 |
| --- | --- |
| 基于 niri `26.04` 首次发布 | `misari-v26.4.0.v1.0` |
| 新增或调整功能 | `misari-v26.4.0.v2.0` |
| 修复该版本的问题 | `misari-v26.4.0.v2.1` |
| 跟进 niri `26.04.1` | `misari-v26.4.1.v1.0` |

## 独立工具版本

- wallpaperd 和 GalScaler 各自按 `X.Y.Z` 发版，tag 分别用 `wallpaperd-vX.Y.Z` 和 `galscaler-vX.Y.Z`。
- GalScaler 桌面端与 Android 共用应用版本和发布 tag。`Cargo.toml`、`mygo.json` 与 tag 中的版本应一致。

## 构建与发布

- 推送组件 tag 自动触发正式发布，也可以手动指定已有的 tag。CI 按前缀选择组件，从 tag 指向的提交构建。同一提交可以对应多个组件的 tag。
- Misari 和 wallpaperd 发布 Arch 包。GalScaler 发布同一 tag 构建的 Arch 包和正式签名的 Android APK。手动发布 GalScaler 时，选择该 tag 运行工作流。
- 日常 Arch 构建和手动 Android 构建的试装包上传到对应的 Actions 运行。
- 查找最新版本时按组件的 tag 前缀筛选。GitHub 的 `Latest` 标记以仓库为单位。

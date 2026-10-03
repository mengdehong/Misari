## 配置

从带注释的[默认配置](../resources/default-config.kdl)开始，个人配置放在 `~/.config/niri/config.kdl`（设置了 `XDG_CONFIG_HOME` 时使用该目录）。保存后自动重载，用 `niri validate` 检查语法。

完整配置、使用说明和内部设计按需查阅[基线版本的上游文档](https://github.com/niri-wm/niri/tree/ed22699d99462f61ab171472d3ea67e844ea580d/docs/wiki)。

## 开发

以下命令在 `niri/` 目录运行；编译所需的系统依赖见上游文档中的 `Getting-Started.md`。

```sh
cargo build --release
cargo test --workspace --exclude niri-visual-tests
```

在现有 Wayland 会话中运行 `./target/release/niri`，可用嵌套窗口试运行；需要更多日志时设置 `RUST_LOG=niri=debug`。

修改配置项时补充 `niri-config` 的解析测试。新增布局操作时同步维护 `src/layout/mod.rs` 中的 `Op` 和适用的 `every_op` 数组。慢速随机测试通过 `RUN_SLOW_TESTS=1` 开启；动画与渲染可用 [niri-visual-tests](../niri-visual-tests/README.md) 检查。

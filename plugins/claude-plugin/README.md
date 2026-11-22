# Claude Code 插件

宿主插件包，实现 Claude Code 会话的读取、原生写入与删除。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 根目录：`~/.claude`，`CLAUDE_CONFIG_DIR` 可改根
- 会话：`projects/<slug>/<sessionId>.jsonl`
- slug：小写盘符，路径中所有非 ASCII 字母数字字符替换为 `-`
- 无独立索引，每行事件自带时间与会话 id

## 读取与写入

- 读取只取 `user` 与 `assistant` 行，跳过 `isSidechain` 子代理与 `isMeta` 元信息行
- 文本 `text`、思考 `thinking`、工具 `tool_use` 与用户侧 `tool_result` 按 `tool_use_id` 配对
- 压缩读 `system/compact_boundary` 与 `isCompactSummary`，兼容旧版 `type: summary`
- 写入重建 `parentUuid` 链、`sessionId` 与 ISO 时间戳；工具结果紧随一条 user 行
- 临时文件写完后改名；备份 `projects/`

## 删除与销毁

- 删除为硬删除：删除 `projects/<slug>/<id>.jsonl`
- 删除与销毁效果相同

## 打开

VS Code 扩展形态启动 VS Code 并把会话工作区作为目录参数传入，没有按会话 id 的深链，会话需在扩展面板选择。CLI 形态在会话 `cwd` 新开控制台启动 claude，带会话 id 时追加 `--resume <id>` 直接续聊。

## 已知限制

- 没有独立会话索引，标题取首条用户文本
- cost/usage 状态行、命令历史与文件快照不迁移
- 验证版本 2.1.325；写入的会话可用 CLI `--resume <id>` 续聊

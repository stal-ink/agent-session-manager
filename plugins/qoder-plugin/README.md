# Qoder 插件

宿主插件包，实现 Qoder 会话的读取、原生写入、删除与销毁。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 索引库：`%APPDATA%/com.qodercn.app.stable/main.sqlite` 的 `chat_sessions`、`chat_session_messages`、`chat_session_sidebar_placements`、`workspaces`
- 正文：`~/.qoder-cn/projects/<slug>/<sessionId>.jsonl`
- 状态：`chat_sessions.archived`、`chat_sessions.deleted_at`、`workspaces.archived`、`workspaces.deleted_at`
- 工作区：按 `root_paths_json` 中任一根目录匹配

## 读取与写入

- 行格式与 Claude Code 同构：`text`、`thinking`、`tool_use`、`tool_result`；`isSidechain` 与 `isMeta` 行跳过
- 会话 id 取文件名或 `sessionId`；标题取会话标题，缺失时取首条用户文本
- 压缩读 `compact_boundary` 与 `isCompactSummary`
- 写入重建 JSONL 行链与 SQLite 索引，整库备份含 WAL/SHM

## 删除与销毁

- 删除为软删除：写 `chat_sessions.deleted_at`
- 销毁删除会话消息、侧栏位置与会话行，并删除对应项目 JSONL

## 打开

启动 Qoder 应用，会话位于侧栏。没有公开的会话级 deep-link，打开只能定位到工作区。

## 已知限制

- 图片保留引用，不搬运二进制
- 验证版本 0.5.7

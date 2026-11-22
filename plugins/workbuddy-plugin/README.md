# WorkBuddy 插件

宿主插件包，实现 WorkBuddy 会话的读取、原生写入、删除与销毁。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 索引：`~/.workbuddy/workbuddy.db` 的 `sessions` 与 `session_usage`，只读 `transport=local`
- 正文：`~/.workbuddy/projects/<压缩工作区路径>/<sessionId>.jsonl`
- 工作区路径压缩：分隔符与冒号替换为 `-`，折叠连续 `-`，盘符转小写，超过 255 字节时截断并追加 djb2 哈希
- `WORKBUDDY_CONFIG_DIR` 可改根

## 读取与写入

- 用户问题从 `<user_query>` 提取，`system-reminder` 上下文不进入 IR
- 思考读 `rawContent`，工具调用按 `callId` 配对结果
- 正文按 JSONL 行序还原，消息数只统计 `message` 行
- 写入插入或更新 `sessions`、重建 `session_usage`，用户文本重新包 `<user_query>`
- 整库备份含 WAL/SHM，同时备份 `projects/` 正文目录

## 删除与销毁

- 删除为软删除：写 `sessions.deleted_at`
- 销毁删除会话与用量行，并删除对应 JSONL 正文
- 没有工作区删除状态

## 打开

通过 `workbuddy://` 协议启动应用，会话位于工作区列表。

## 已知限制

- 写入不生成 providerData 追踪字段，工具调用按名称、参数与结果重建
- 验证版本 5.9.3

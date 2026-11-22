# Kiro 插件

宿主插件包，实现 Kiro 会话的读取、原生写入与删除。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 根目录：`~/.kiro`
- 会话：`sessions/<hash16>/sess_<uuid>/session.json` 与 `messages.jsonl`
- 索引：`session-index/<hash16>.jsonl` 与 `.index/index.json`
- 工作区目录：`hash16 = sha256(全小写正斜杠路径)` 前 16 位
- 面板状态：`%APPDATA%/Kiro/User/workspaceStorage/*/state.vscdb` 的 `ItemTable['kiro.kiroAgent']`

## 读取与写入

- 消息按 `messages.jsonl` 行序还原，`turn_start`/`turn_end` 划分用户请求
- `user`、assistant Say/Reasoning、`tool_call`/`tool_result` 进入 IR；审批、usage、上下文注入与子代理事件忽略
- 写入在临时目录构建后改名；同一 turn 共享 `executionId` 与 `turnId`
- 写入后更新 `session-index/<hash16>.jsonl` 与 `.index/index.json`，重建条目并刷新目录哈希
- 图片写 `{data,mimeType}`；工具调用参数必须为对象，非对象输入包成 `{"value":...}`
- 备份 `sessions/`、`session-index/`、`spec-sessions/`，写入前需 Kiro 完全关闭

## 删除与销毁

- 删除为硬删除：移除整个会话目录并向索引追加 `remove`
- 删除与销毁效果相同

## 打开

启动 Kiro 时传入目标工作区路径，会话进入该工作区的 Session History。只启动 `--agents` 会打开没有工作区的 Agent Focus。

## 已知限制

- 没有归档与工作区删除状态；目录存在即活跃，历史会话按已归档展示
- 不迁移 `reasoningSignature`、审批流、usage/context 元数据、子代理转录、快照与外部 `tool-outputs`
- 工具调用不生成 `kind`/`actionType`，大结果内联
- 写入与续聊尚未在实际使用中验证
- 验证版本 1.4.121

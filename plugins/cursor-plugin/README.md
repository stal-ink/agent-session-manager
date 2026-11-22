# Cursor 插件

宿主插件包，实现 Cursor 会话的读取、原生写入与删除。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 主库：`%APPDATA%/Cursor/User/globalStorage/state.vscdb`
- 列表：`composerHeaders`，排除 `isSubagent=1`
- 消息：`cursorDiskKV` 的 `bubbleId:<composerId>:<bubbleId>`，时间线按 `createdAt` 与主键顺序还原
- 工作区：`User/workspaceStorage/<id>/workspace.json` 的 `folder`
- 状态：`composerHeaders.isArchived`；子代理 `isSubagent=1` 排除

## 读取与写入

- `composerHeaders` 提供标题、归档状态与顺序；`composerData` 的 `fullConversationHeadersOnly` 定义消息顺序
- 气泡 type 1 为用户、type 2 为助手；思考写 `thinking.text`
- 工具调用写 `toolFormerData` 的 name、params 或 rawArgs、result、status
- 写入绑定工作区；无匹配工作区时新建目录与 `workspace.json`
- 整库备份含 WAL/SHM，写入前需 Cursor 完全关闭

## 删除与销毁

- 删除为硬删除：事务内删除 `composerHeaders`、`composerData`、`bubbleId` 与 `checkpointId` 键
- 删除与销毁效果相同

## 打开

启动 Cursor，会话位于 Composer 历史中。

## 已知限制

- 不生成 checkpoint，无法在 Cursor 内回退到历史检查点
- 不伪造 `toolCallBinary`，工具卡片按名称与参数展示
- 验证版本 3.29.15

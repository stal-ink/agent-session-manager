# ZCode 插件

宿主插件包，实现 ZCode 会话的读取、原生写入、删除与销毁。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 会话库：`~/.zcode/cli/db/db.sqlite` 的 `session`、`message`、`part` 等表
- 任务索引：`~/.zcode/v2/tasks-index.sqlite` 的 `tasks`、`projects`
- 执行目录：`~/.zcode/cli/exec/<session_id>/`
- 状态：`tasks.archived`、`tasks.deleted`、`tasks.cli_deleted`、`projects.deleted`、`projects.deleted_at`

## 读取与写入

- 消息按 `message.sequence` 排序，part 按 `part.sequence` 归组
- part 类型 `text`、`reasoning`、`tool`、`file` 映射到 IR；工具状态取 `state.status`
- 压缩读 `part.type=compaction` 的 `summaryMessageId`
- 删除与写入均在事务内完成

## 删除与销毁

- 删除为软删除：写 `tasks.deleted`、`tasks.cli_deleted` 与 `session.time_deleted`
- 销毁逐表删除会话记录、任务索引行与 `cli/exec/<id>/` 目录；`permission` 为项目级授权，不随会话删除

## 打开

启动 ZCode IDE，单实例时聚焦已有窗口，会话在任务列表中。`zcode://workspace/open` 会触发外部文件夹信任确认，打开动作不传会话。

## 已知限制

- 验证版本 3.17.2

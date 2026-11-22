# Kimi Code 插件

宿主插件包，实现 Kimi Code 会话的读取、原生写入与删除。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 根目录：`~/.kimi-code`，`KIMI_CODE_HOME` 可改根
- 会话：`sessions/<bucket>/<session_id>/state.json` 与 `agents/main/wire.jsonl`
- 索引：目录扫描列表；`session_index.jsonl` 辅助续聊，无中心数据库
- 工作区目录：`wd_` + 小写 basename slug + `_` + `sha256(正斜杠保留大小写路径)` 前 12 位
- 归档：`state.json.archived`

## 读取与写入

- 按 wire 重放：用户消息取 `context.append_message` 且 `origin.kind=user`，assistant 以 step 为单位
- `undo`、`clear`、`apply_compaction` 生效；usage、token、permission 事件忽略
- 写入 `metadata`、`turn.prompt`、`context.append_message`、`step.begin`、`content.part`、`tool.call`、`tool.result`、`step.end`、`turn.ended`
- 标题写 `titleKind="custom"`；`locks/sessions/` 有锁时拒绝写入
- 覆盖时删除同 id 旧目录后按目标工作区重建
- 备份 `sessions/`、`session_index.jsonl` 与 `projects.json`

## 删除与销毁

- 删除为硬删除：递归删除会话目录，并向 `session_index.jsonl` 追加 `deleted`
- 删除与销毁效果相同

## 打开

在会话 `cwd` 执行 `kimi -r <id>`；无 id 时在该工作区新开会话。

## 已知限制

- wire 协议 1.5 属客户端内部实现
- 手写会话能否被 `kimi -r` 正确续聊、图片 media part 的真实形状尚未在实际使用中验证
- 会话锁按 PID 与开机时间判活，过期锁不阻塞写入
- 验证版本 2.6.3

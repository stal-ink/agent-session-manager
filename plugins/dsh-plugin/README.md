# DSH 插件

宿主插件包，实现 DeepSeek Harness 会话的读取、原生写入、删除与销毁。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 根目录：`~/.dsh`，`DSH_HOME` 可改根
- 会话：`sessions/<projectKey>/session-<uuid>/session.v4.jsonl.zstd`
- 索引：`storages/workspace.json`、`session_projcache/`、`message_feedback.json`
- 项目目录键：分隔符与冒号折叠为 `-`，其余不安全字符按 `~XXXX` 转义，首尾加 `--`，最长 251 字符

## 读取与写入

- 物理格式为多帧 zstd；第一帧只含 header，后续每批事件一帧并带 checksum；逻辑行 `seq` 从 0 连续
- 只保留来源为人类的 `user/message`；assistant 内容块 `reasoning`、`text`、`tool-call` 进入 IR
- 压缩读 `compact-checkpoint`，兼容旧版 `plugin/compact`
- 写入按 turn/step 组织；assistant/message 必须带非空 `id`，事件必须带结算字段 `stream`
- 压缩写 `compaction/start`、`compaction/summary`、替换检查点与 `compaction/end`
- 新会话挂接到 `workspace.json` 对应工作区的 `sessionIds` 首位
- 备份 `sessions/` 与 `storages/`，写入前需 `DeepSeek Harness.exe` 完全关闭

## 删除与销毁

- 删除为软删除：向 `workspace.json` 的 `archivedSessionIds` 追加会话 id
- 销毁删除会话目录、投影缓存、`session_projcache.json` 与 `message_feedback.json` 记录，并从工作区索引摘除

## 打开

通过开始菜单快捷方式或安装目录启动桌面应用。

## 已知限制

- system-reminder、runtime-context 等注入行不进入 IR
- 不生成 request/header、approval、job、usage 与标题 LLM 请求事件
- 写入结果符合 session format v4 行格式，可经 `Session.fromRestore` 还原；界面续聊尚未验证
- 验证版本 0.2.2-rc.3（session format v4）

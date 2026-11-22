# Trae CN 插件

宿主插件包，实现 Trae CN 会话的只读读取。协议与打包规则见 [插件架构](../../docs/plugin-architecture.md)。

## 存储

- 数据根：`%APPDATA%\Trae CN`，英文版为 `%APPDATA%\Trae`；`TRAE_DATA_DIR` 可改根
- 数据库：`ModularData\ai-agent\database.db`，另有 `database.db-wal` 与 `database.db-shm`
- 格式：SQLCipher 4，AES-256-CBC，page_size 4096，每页 reserve 80 字节
  - 第 1 页前 16 字节为 salt，密文从偏移 16 开始；其余页从 0 开始
  - 每页 IV 位于 4016..4032，HMAC-SHA512 位于 4032..4096
  - HMAC 密钥由加密密钥经 PBKDF2-HMAC-SHA512 2 轮派生，salt 需异或 `0x3A`

## 密钥

- 密钥是 Trae 构建期固定值，同一构建跨机器、跨重启通用
- 获取在 `plugin.register` 之后、`list`/`load` 内部惰性执行；注册前不退出进程
- 获取顺序：
  1. `data/key-cache.json` 缓存（必须通过首页 HMAC 校验，且 salt 匹配）
  2. 内置候选密钥表
  3. 可选的进程内存扫描（需要 Trae 正在运行）
- 每次获取最多 3 轮，单轮扫描有超时，总时长小于 manifest 的 `timeout_ms`
- 失败通过 JSON-RPC error 返回，消息包含失败阶段、重试次数和数据库路径
- 缓存写入使用临时文件加原子替换；写失败只产生 `warnings`，不影响本次读取

`data/settings.toml` 是插件自有配置，宿主不读取：

```toml
[key_acquisition]
cache = true
memory_scan = false
retry_attempts = 3
attempt_timeout_ms = 8000
total_timeout_ms = 45000
```

- `memory_scan = true` 才会读取运行中 Trae 进程的内存；默认关闭
- 升级插件会切换版本目录，缓存与配置会回到包内默认值，可接受重建

## 版本检查

- 渠道 `trae-cn-desktop`，GET Trae CN 官方 `package/check_update`
- 使用匿名固定 `mid` / `did`，不读取本机机器码；请求参数固定 `appVersion=0.0.1`
  以强制返回最新清单
- 只解析 `data.appVersion`（产品版本 3.4.x）；`data.manifest.win32.version`
  是构建号（2.3.x），不参与比较
- manifest 声明通用规则 `parser = "json"`、`paths = ["data.appVersion"]`，
  由宿主通用 JSON 路径解析，不需要产品专用 parser

## 读取

- 会话列表来自 `chat_session`，工作区来自 `project`
- 消息来自 `history_v2` 事件流：`user_input`、`llm_default` 与工具行按 `id` 排序
- 用户正文取 `<user_input>` 段，缺失时剥掉 `<system-reminder>` 等注入块
- 助手文本、`tool_calls` 与 `extra_info.reasoning_content` 分别映射为文本、工具调用与思考
- 工具结果按 `tool_call_id` 回填；`<toolcall_result>` 与 `<shell_output>` 取正文
- 压缩摘要读取 `summary` 与 `summarized_above`（当前版本未见写入）
- 状态映射按宿主四态：
  - 已删除：`chat_session.deleted_at != 0`，仍会出现在列表中
  - 工作区归档/删除：`project.is_archived`、`project.deleted_at` 或项目记录缺失
  - 会话归档：`chat_session.extra` 中的 `archived` / `is_archived`；当前构建
    `sidebar archive is disabled`，实际没有会话级归档数据
- 读取时把主库逐页解密到临时文件，再应用 WAL 中最后一次提交之前的帧；临时文件用完即删
- 单条会话无法解析时跳过并返回 `warnings`；数据库不存在、整库或密钥不可用时返回错误

## 删除与写入

插件只读：不支持删除、销毁与转接写入。原因：

- 写入需要自行重新加密每一页并重算 HMAC，同时维护 WAL，等于实现一个存储引擎
- 一个可被 Trae 打开的会话要同时维护 `chat_session`、`chat_turn`、`chat_message`、
  `chat_message_general/task`、`history_v2`、`task`、`session_project` 的 id 与 JSON 关联
- Trae 的 schema 是内部实现，没有稳定导入格式；字段不匹配可能导致会话不显示或库打不开
- 宿主安全要求写入前关闭目标进程、整库备份并可回滚，测试必须使用独立 profile

读取已经覆盖列表、载入与 Markdown 导出。

## 打开

通过 Trae CN 安装时注册的 `trae-cn` 协议定位 exe 并启动，支持自定义安装目录；
不支持按会话 id 深链。

## 已知限制

- 只读，不能作为转接目标
- Trae 不提供打开会话清单，manifest 声明 `open_session_ids = false`，`active` 固定返回 `false`
- WAL 应用不校验校验和，以最后一次提交为边界
- 解密后的临时明文库位于系统临时目录，插件进程退出前删除
- 密钥与 Trae 构建版本绑定，升级后可能需要更新内置候选或打开 `memory_scan`
- 会话内注入的 `<system-reminder>`、`<environment>` 等块不进入 IR
- 已验证版本 3.4.22（构建号 2.3.105403）；会话列表与历史消息可正常读取

## 致谢

SQLCipher 页格式、`x'<key+salt>'` 密钥形式与内存扫描思路来自社区：

- TRAE 官方中文社区：[topic/18248](https://forum.trae.cn/t/topic/18248)、[topic/18351](https://forum.trae.cn/t/topic/18351)
- [bigmanBass666/trae-db-decrypt](https://github.com/bigmanBass666/trae-db-decrypt)
- 同源思路：[ylytdeng/wechat-decrypt](https://github.com/ylytdeng/wechat-decrypt)

本插件的 Rust 读取、解密、WAL 应用与 IR 映射为独立实现。

# Codex 插件

宿主插件包，实现 Codex 的读取、app-server 原生写入、删除与销毁。读取器直接解析 rollout 与状态库；写入、覆盖与删除经过 app-server。代码入口是本包的 `src/writer.rs`，读取与兼容逻辑在 `src/parse.rs`。

## 1 问题

Codex 的 CLI、VS Code 扩展与桌面端共用 `~/.codex`。线程索引在 `state_*.sqlite.threads`，正文在 `sessions/YYYY/MM/DD/rollout-*.jsonl`，归档在 `archived_sessions/`，归档状态读 `threads.archived`。三个客户端都可能启动 app-server，另有常驻的 managed daemon 与 `thread-writer-locks/` 写锁。

直接手写 SQLite 与 rollout 要同时维护索引、分页投影、通知和锁。任一环节与运行中的 Codex 冲突，轻则会话不显示，重则覆盖其他客户端的并发写入。这类内部 schema 还会随版本迁移。

app-server 提供了线程级接口。写入由 Codex 自己完成，本工具只提交语义内容，不复制其索引与投影逻辑。

## 2 设计目标

- 原生写入由 Codex 自己落盘，保持三端共享状态一致。
- 写入失败不留下半成品线程，旧线程保持原状。
- 读取不依赖 app-server，CLI 与扩展都不可用时仍能列出会话。
- 协议字段变化只影响写入器，不扩散到统一 IR。

非目标：

- 不管理 Codex 的登录、模型供应商与凭据。
- 不把 app-server 作为常驻服务托管。
- 不重建 Codex 的全部界面事件，只补历史可见性需要的部分。

## 3 进程模型

app-server 可以按 stdio 子进程运行，也可以用 `app-server --listen ... --managed-daemon` 常驻。本工具只使用 stdio 形式。

每次写入或删除时，`Server::connect` 启动一个 `codex app-server` 子进程。stdin 写请求，stdout 读响应，stderr 丢弃，Windows 下隐藏控制台窗口。操作完成后关闭 stdin，最多等待 3 秒；进程未退出则对本次启动的进程树执行 `taskkill /T /F`。本工具不连接 Codex 客户端已有的 managed daemon，也不结束其他 Codex 进程。

候选可执行文件按顺序尝试：

| 顺序 | 来源 | 命令 |
|---|---|---|
| 1 | PATH 上的 Codex CLI | `cmd /c codex app-server` |
| 2 | VS Code 扩展内置二进制 | `~/.vscode{,-cn}/extensions/openai.chatgpt-*/bin/windows-x86_64/codex.exe app-server` |
| 3 | CLI 的 app-server-daemon 包 | `~/.codex/packages/app-server-daemon/current/bin/codex.exe app-server` |
| 4 | 桌面端安装目录 | `%LOCALAPPDATA%/OpenAI/Codex/bin/<版本>/codex.exe app-server` |

第一个启动并通过握手的候选被采用。握手发送 `initialize`，携带 `clientInfo` 与 `capabilities.experimentalApi=true`，随后发送 `initialized` 通知。协议是逐行 JSON-RPC，对象不带 `jsonrpc` 字段，客户端按自增 `id` 匹配响应。

本工具不按版本号筛选候选。

## 4 写入流程

```text
transfer::execute
  → writer::write
    → 启动 app-server 并完成握手
    → thread/start {cwd, historyMode: "legacy"}
    → thread/inject_items，每批 200 条
    → thread/name/set
    → 覆盖时删除旧线程
    → 关闭 app-server
    → augment_history 补 UI 事件
```

主要请求：

| 方法 | 参数 | 超时 | 作用 |
|---|---|---|---|
| `initialize` | `clientInfo`、`experimentalApi` | 20 秒 | 握手 |
| `thread/start` | `cwd`、`historyMode` | 60 秒 | 新建线程 |
| `thread/inject_items` | `threadId`、`items` | 120 秒 | 注入历史 |
| `thread/name/set` | `threadId`、`name` | 30 秒 | 设置标题 |
| `thread/delete` | `threadId` | 60 秒 | 删除新线程或旧线程 |

新线程固定使用 `historyMode: "legacy"`。legacy 客户端直接读 rollout，不依赖 `thread_history_*.sqlite` 的分页投影。早期版本由本工具创建的 paginated 线程在再次转接时判定为需要重建，打开前也会迁移到 legacy。

覆盖流程先创建并填充新线程。全部成功后才删除旧线程，台账改记新 id。任一步失败时删除本次新建的线程，旧线程不动。

标题截断到 120 个字符。工作区取转接对话框的值，为空时回退到源会话的 `cwd`。

## 5 数据映射

| IR | Codex item |
|---|---|
| 用户文本 | `message` / `user` / `input_text` |
| 用户图片 | `input_image`，仅接受内联 `data:image/` URL |
| 用户文件 | `input_text`，内容为 `[文件：路径]` |
| assistant 文本 | `message` / `assistant` / `output_text` |
| assistant 思考 | `reasoning` 的 `summary[].summary_text` |
| 工具调用 | `function_call`，参数为 JSON 字符串 |
| 工具结果 | `function_call_output`，按 `call_id` 配对 |
| 压缩摘要 | `message` / `developer` / `input_text` |

图片只搬运内联 data URL。远程 URL、本地路径与其他引用降级为 `[图片：地址]` 文本，不下载图片，不读取本地文件。Codex app-server 拒绝 `input_image` 中的远程 URL，因此这一限制在写入前处理。

压缩摘要只在 `tail_start_index == 0` 时写入。设置页开启「仅注入最近压缩之后的消息」后，转接层把消息截到压缩边界并把下标归零，摘要承载压缩前的上下文。全量转接没有这个独立摘要 item，边界之前的原始消息已经全部写入。

## 6 两套历史

Codex 的模型历史与界面历史不是同一份数据。

- `response_item` 是模型可见历史，使用 Responses API 的 item 结构。`thread/inject_items` 写入这里。
- `event_msg` 是界面事件流。legacy 历史构建读取 `user_message`、`agent_message`、`agent_reasoning` 等事件。

只写 `response_item` 时，模型能看到注入内容，但 Codex CLI 与桌面端的历史面板可能只显示后来原生生成的 turn。本工具因此在写入后补一份 `event_msg`。

`augment_history` 的规则：

1. 找到 rollout 中首个原生 `task_started`。
2. 若它之前已经存在 `user_message`，视为已补过，不再修改。
3. 对之前的每个 user/assistant message 与 reasoning，生成对应的 `event_msg`。
4. 跳过 `parse::is_injection` 判定的注入文本。
5. 把事件插到首个 `task_started` 之前；没有原生 turn 时追加到文件末尾。
6. 原子重写 rollout。

补写只覆盖文本与思考。工具调用保留在模型历史中，界面不生成工具卡片。

打开本工具早期写出的线程时，`ensure_legacy` 先把 `originator=agent-session-manager` 且 `history_mode=paginated` 的行改为 legacy，再执行同一套事件补写。这是本工具直接修改 Codex 状态库的唯一位置，限定在自身创建的线程，数据库忙或缺少列时不阻断打开。

## 7 备份、失败与删除

转接引擎在调用写入器之前仍对 Codex 存储做整份备份。Codex 的 `restore_on_error=false`，写入失败不执行整库恢复以避免覆盖三端共享存储的并发改动，只删除 app-server 本次新建的线程；备份保留为人工恢复点。

删除与销毁调用 `thread/delete`。Codex 的删除是物理删除，删除 rollout、线程索引与派生状态。

本插件声明 `requires_exclusive=false`，`target_running` 返回 false。写入不按 `codex.exe` 映像名排他，因为 CLI、VSIX、桌面端与 managed daemon 可能同时存在，而 app-server 自身负责共享状态协调。

## 8 打开

打开会话前先执行 legacy 迁移与事件补写，再按形态启动：

| 形态 | 打开方式 |
|---|---|
| CLI | 在会话 `cwd` 执行 `codex resume <id>` |
| VS Code 扩展 | 先打开工作区，再尝试 `vscode://openai.chatgpt/local/<id>` |
| 桌面端 | `codex://threads/<id>` |

VS Code 深链未注册时工作区已经打开，会话需要从扩展面板选择。app-server 不可用只影响写入与删除，不影响读取和启动客户端。

## 9 安全边界

- 源端存储只读。
- app-server 只通过 stdin/stdout 通信，不监听端口。
- 子进程由本次操作启动，退出时只结束该进程树。
- 本工具不读取或写入 Codex 的登录凭据。
- 写入失败返回 app-server 的错误正文，不伪造成功。

模型请求返回 401 属于 Codex 的供应商配置，不是转接错误。

## 10 已知限制

- app-server 协议仍标记 experimental，字段和方法可能随 Codex 版本变化。
- `event_msg` 补写依赖 rollout 的内部事件结构，不是公开写入接口。
- 工具调用卡片不重建，界面只显示文本与思考历史。
- 三端列表的刷新时机由各自客户端决定，写入成功后不保证立即出现在每个窗口。
- 本工具不托管 managed daemon，也不复用其连接。
- `state_*.sqlite` 的读取按列名动态解析；schema 大幅变化时需要更新 fixture 与解析器。

## 11 验证状态

单元测试覆盖 item 映射、工具配对、压缩摘要、远程图片文本降级、legacy 事件补写与幂等性。

CLI 与桌面端的转接、打开与历史显示已验证。VS Code 扩展深链与三端列表刷新时机未单独验证。

## 12 参考

- 读取与兼容：本包 `src/parse.rs`
- 写入与事件补写：本包 `src/writer.rs`
- 存储位置与格式：本文第 1 节
- 宿主架构：[docs/architecture.md](../../docs/architecture.md)
- 插件协议与打包：[docs/plugin-architecture.md](../../docs/plugin-architecture.md)

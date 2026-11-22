# 插件开发指南

## 1 最终产物

一个可导入的插件包必须包含：

```text
<id>-<version>.zip
├─ plugin.toml           manifest，包根
├─ icon.png              512×512 产品图标
├─ bin/
│  └─ agent-plugin-<id>.exe
├─ data/                 可选，插件私有文件，宿主不解析
└─ <形态图标>             可选，manifest 明确引用时
```

`execution.command` 指向包内的 exe。exe、图标和 manifest 是导入的最低要求。

## 2 新建 Cargo 包

在仓库内开发时，目录固定为 `plugins/<id>-plugin/`：

```text
plugins/<id>-plugin/
├─ Cargo.toml
├─ plugin.toml.in
├─ icon.png
├─ data/                 可选
├─ src/
│  ├─ lib.rs             存储读取与写入
│  └─ main.rs            PluginAgent 实现与 serve()
└─ tests/
   └─ protocol.rs        自身 exe 的 JSON-RPC 测试
```

`Cargo.toml` 示例：

```toml
[package]
name = "agent-plugin-example"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "agent-plugin-example"
path = "src/main.rs"

[dependencies]
agent-core = { path = "../../agent-core" }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

只在需要 SQLite 时启用 `agent-core` 的 `sqlite` feature。插件不允许依赖 `agent-session-manager`、Slint、UI 或宿主 `assets/`。

仓库内新增插件还要：

1. 把 `plugins/<id>-plugin` 加入根 `Cargo.toml` 的 workspace `members`。
2. 把 id 加入 `package-plugins.cmd` 的 `TARGETS` 和 `validate` 列表。
3. 在 `plugins/<id>-plugin/README.md` 记录存储路径、读写格式、删除语义、限制和已验证的上游版本。

### 2.1 在宿主仓库外开发

插件不依赖宿主源码，可以单独建 Cargo 项目：

```text
example-plugin/
├─ Cargo.toml
├─ plugin.toml
├─ icon.png
├─ bin/agent-plugin-example.exe
├─ src/main.rs
└─ src/lib.rs
```

```toml
[package]
name = "agent-plugin-example"
version = "0.1.0"
edition = "2021"

[dependencies]
agent-core = { path = "../agent-core" }
anyhow = "1"
```

构建后手动完成打包：

```sh
cargo build --release
```

1. 把 exe 放到包内 `bin/`。
2. 复制 manifest 模板，填入 `version`。
3. 用 `Get-FileHash bin/agent-plugin-example.exe -Algorithm SHA256` 计算 `sha256`。
4. 把 manifest 保存为包根 `plugin.toml`。
5. 放入 512×512 `icon.png`，需要时放入 `data/`。
6. 把包根内容打成 zip，通过界面或 `plugin import` 导入。

这种方式不需要修改 app.exe，也不需要把插件加入宿主 workspace。

## 3 实现 PluginAgent

`agent-core` 的 `PluginAgent` 是唯一插件入口。最小实现：

```rust
use std::path::PathBuf;

use agent_core::ir::{Session, SessionSummary};
use agent_core::sdk::{serve, PluginAgent, PluginDescriptor};
use anyhow::{bail, Result};

struct Example;

impl PluginAgent for Example {
    fn list(&self) -> Result<Vec<SessionSummary>> {
        Ok(Vec::new())
    }

    fn load(&self, id: &str) -> Result<Session> {
        bail!("尚未实现读取会话：{id}")
    }

    fn write(&self, _session: &Session, _cwd: &str, _overwrite: Option<&str>) -> Result<String> {
        bail!("该插件只读，不支持转接写入")
    }

    fn delete(&self, id: &str) -> Result<()> {
        bail!("该插件只读，不支持删除：{id}")
    }

    fn active(&self, _id: &str) -> Result<bool> {
        Ok(false)
    }

    fn backup_paths(&self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

fn main() -> Result<()> {
    let mut descriptor = PluginDescriptor::new(
        "example",
        "Example Agent",
        "example.exe",
        "example",
        env!("CARGO_PKG_VERSION"),
    );
    // 上面的示例实现是只读插件，三项必须显式关闭。
    descriptor.can_write = false;
    descriptor.requires_exclusive = false;
    descriptor.restore_on_error = false;
    serve(&Example, &descriptor)
}
```

`PluginDescriptor::new` 默认三项都是 `true`。只读插件必须在 `main` 中按上面的方式显式关闭；可写插件按 3.3 设置。

`descriptor` 的 `id`、`label`、`process`、`launch`、`version` 必须与 manifest 一致，导入探测会逐项比对。

### 3.1 必须实现的方法

| 方法 | 要求 |
|---|---|
| `list` | 返回所有可见会话摘要；单个坏会话不能拖垮整批时，改为覆写 `list_with_warnings` |
| `load` | 按 id 返回完整 IR；找不到会话必须返回错误 |
| `write` | 返回目标会话 id；`overwrite` 非空时覆盖已有目标；只读插件可直接返回错误 |
| `delete` | 按该 agent 原生命义删除或标记删除 |
| `active` | 查询会话是否仍在客户端打开；查询失败必须返回错误 |
| `backup_paths` | 返回写入或删除前需要备份的文件和目录；查询失败必须返回错误 |

### 3.2 可选覆写的方法

| 方法 | 用途 |
|---|---|
| `destroy` | 物理删除；默认复用 `delete`。软删除插件必须实现 |
| `list_with_warnings` | 单条会话解析失败时返回 `AgentListing.warnings` |
| `prepare_open` | 打开会话前修正产物 |
| `ensure_closable` | 关闭客户端前检查会话锁等条件 |
| `open_session_ids` | 返回客户端当前打开的会话 id；无法读取返回 `None` |
| `needs_rewrite` | 已存在的目标会话是否必须重建 |

单条会话坏了但其余会话可读时，不要让整个 `list` 失败：

```rust
use agent_core::adapter::AgentListing;
use agent_core::protocol::PluginWarning;

fn list_with_warnings(&self) -> Result<AgentListing> {
    let mut sessions = Vec::new();
    let mut warnings = Vec::new();
    // 逐条读取；坏掉的会话跳过并记录原因
    warnings.push(PluginWarning {
        kind: "parse_skipped".into(),
        message: "会话 abc 无法解析：字段缺失".into(),
        session_id: Some("abc".into()),
    });
    Ok(AgentListing { sessions, warnings })
}
```

整库不可读、数据库不存在、密钥获取失败等影响全部会话的情况，仍然从 `list` 或 `load` 返回 `Err`。

### 3.3 可写插件

要成为转接目标，必须同时满足：

- manifest 的 `can_write = true`
- `capabilities.write = true`
- 实现 `write`，返回目标会话 id
- 实现 `backup_paths`，返回写入前需要备份的数据库、`-wal`、`-shm` 和正文目录
- 按目标应用行为设置 `safety.requires_exclusive` 与 `safety.restore_on_error`

宿主负责关闭目标进程、复制备份、失败回滚。插件不能指定备份目的地，也不能自己回滚。

删除与销毁：

- 支持删除时设置 `capabilities.delete = true` 并实现 `delete`
- `delete` 只是标记删除时，设置 `delete_is_physical = false`，同时实现 `destroy` 做物理删除
- `delete` 本身就是物理删除时，设置 `delete_is_physical = true`，`destroy` 可以复用 `delete`

## 4 manifest 模板

把下面内容保存为 `plugin.toml.in`。这是只读插件的基线模板，`@VERSION@` 和 `@SHA256@` 由打包脚本替换。需要转接写入时，再把 `can_write`、`capabilities.write/delete/destroy` 与 `safety` 改成真实能力，并实现对应方法。

```toml
schema = 2
id = "example"
label = "Example Agent"
version = "@VERSION@"
protocol = 1
ir_version = 1
min_host = "0.17.0"
process = "example.exe"
launch = "example"
icon = "icon.png"
can_write = false
sha256 = "@SHA256@"
group_color = "#4a7bd0"

[execution]
mode = "ephemeral"
command = "bin/agent-plugin-example.exe"
args = []
timeout_ms = 30000

[capabilities]
list = true
load = true
write = false
delete = false
destroy = false
active = true
backup_paths = true

[safety]
requires_exclusive = false
restore_on_error = false
delete_is_physical = false

[integration]
tracks_process = true
close_process = ""
close_mode = "process"
open_session_ids = false
archive_from_open_ids = false

[[variants]]
id = "example-cli"
label = "Example CLI"
kind = "cli"
default_open = true

[variants.open]
strategy = "command"
command = "example"
args = []
session_args = ["--resume", "{session_id}"]

[variants.update]
channel = "example-cli"
version_url = "https://example.com/updates/latest.json"
parser = "json"
paths = ["version"]
download_url = "https://example.com/download"

[[variants.detectors]]
type = "command_version"
command = "example"
args = ["--version"]
```

固定结构字段必须显式写全。布尔值即使为 `false`、数组即使为空也要写出，不要依赖宿主默认值。新增插件从模板复制后改值，不要自行删减字段。

### 4.1 顶层字段

| 字段 | 必填 | 说明 |
|---|---|---|
| `schema` | 是 | 固定为 `2` |
| `id` | 是 | 小写 `[a-z][a-z0-9-]{1,31}`，与目录和 descriptor 一致 |
| `label` | 是 | 产品显示名 |
| `version` | 是 | 插件版本，安全文件名；打包时从 Cargo 版本注入 |
| `protocol` | 是 | 当前为 `1`，必须与宿主一致 |
| `ir_version` | 是 | 当前为 `1`，必须与宿主一致 |
| `min_host` | 是 | 最低宿主版本，三段语义版本 |
| `process` | 是 | 用于进程检测和关闭的映像名 |
| `launch` | 是 | 旧入口和显示用的默认启动目标 |
| `icon` | 是 | 相对包根的图标路径，通常为 `icon.png` |
| `can_write` | 是 | 是否可作为转接目标 |
| `sha256` | 是 | exe 的 SHA-256，打包脚本计算 |
| `group_color` | 是 | 产品分组色，十六进制颜色 |
| `execution` | 是 | 短命进程命令与超时 |
| `capabilities` | 是 | 协议能力开关 |
| `safety` | 是 | 排他、回滚与删除语义 |
| `integration` | 是 | 进程跟踪、打开会话与归档推断 |
| `variants` | 是 | 一个或多个产品形态 |

### 4.2 execution

| 字段 | 必填 | 说明 |
|---|---|---|
| `mode` | 是 | 当前只接受 `ephemeral` |
| `command` | 是 | 相对包根的 exe 路径 |
| `args` | 是 | 启动参数，没有写空数组 |
| `timeout_ms` | 是 | 单请求超时，范围 100 到 120000 |

### 4.3 capabilities

`list`、`load`、`write`、`delete`、`destroy`、`active`、`backup_paths` 全部必填，与协议方法一一对应。`can_write = true` 时 `write` 必须为 `true`。

### 4.4 safety

| 字段 | 必填 | 说明 |
|---|---|---|
| `requires_exclusive` | 是 | 写入前是否要求目标进程退出 |
| `restore_on_error` | 是 | 写入失败是否用整库备份回滚 |
| `delete_is_physical` | 是 | `delete` 是否本身就是物理删除 |

### 4.5 integration

| 字段 | 必填 | 说明 |
|---|---|---|
| `tracks_process` | 是 | 是否有可探测的桌面进程 |
| `close_process` | 是 | 需要关闭的映像名；空串表示使用 `process` |
| `close_mode` | 是 | `process` 或 `check_only` |
| `open_session_ids` | 是 | 是否支持读取客户端当前打开的会话 id |
| `archive_from_open_ids` | 是 | 未打开的会话是否按已归档展示 |

### 4.6 形态

| 字段 | 必填 | 说明 |
|---|---|---|
| `id` | 是 | 形态 id，小写，可含 `-`、`_`、`.` |
| `label` | 是 | 形态显示名 |
| `kind` | 是 | `desktop`、`cli` 或 `vsix` |
| `default_open` | 是 | 是否默认打开；最多一个形态为 true |
| `open` | 是 | 打开策略 |
| `icon` | 否 | 形态图标，相对包根 |
| `update` | 否 | 版本检查渠道 |
| `detectors` | 否 | 安装检测器列表 |

`open.strategy` 取值：

- `registry`：通过注册表协议启动
- `command`：执行 PATH 或绝对路径上的命令
- `candidates`：依次尝试 `candidates` 中的路径
- `vscode_workspace`：打开工作区，再按 `session_uri` 打开会话
- `uri`：打开 URI

`open.command` 支持 `{cwd}` 和 `{session_id}` 占位。`session_args` 在带会话 id 时追加。`candidates` 只在 `candidates` 策略中使用。`session_uri` 只在工作区或 URI 策略中需要。

### 4.7 update

| 字段 | 必填 | 说明 |
|---|---|---|
| `channel` | 是 | 渠道 id，必须全插件唯一 |
| `version_url` | 是 | Agent 产品版本检查页，抓取版本号用；不是插件包下载地址 |
| `parser` | 是 | `json`、`key`、`regex`、`html_regex` 或 `text` |
| `download_url` | 是 | Agent 产品下载页，给用户点开；不是插件包下载地址 |
| `paths` | 按格式 | `json` 的点分路径，按顺序尝试 |
| `key` | 按格式 | `key` 的顶层键名 |
| `patterns` | 按格式 | `regex` 或 `html_regex` 的正则，按顺序尝试 |
| `group` | 否 | 正则捕获组，默认 1；0 表示整个匹配 |
| `segments` | 否 | 只取前 N 段版本，0 表示不截取 |
| `min_parts` | 否 | 至少需要的数字段数，默认 2 |

宿主只实现通用解析格式，不按产品名分支。新增插件不需要修改宿主 parser。旧模板里的 `url =` 仍按 `version_url` 读取，新插件统一写 `version_url`。

版本号解析只负责取出候选值，比较前会去掉前导 `v`/`V`。发布页、构建号等不是产品版本的字段不要写进路径或正则。

### 4.8 detectors

`type` 必填，支持：

- `desktop_app`：注册表、开始菜单快捷方式和常见安装目录
- `vscode_extension`：VS Code 和 VS Code CN 扩展目录
- `npm_package`：全局 npm 包
- `native_binary`：固定路径二进制
- `command_version`：执行命令读取版本，限时 5 秒

可用字段：`registry_keys`、`exclude_prefixes`、`exe`、`shortcut_names`、`state_db`、`roots`、`prefix`、`root`、`package`、`path`、`command`、`args`。只写该类型实际使用的字段，未知字段不会参与检测。

## 5 JSON-RPC 契约

协议版本为 1，一行一条 UTF-8 JSON，stdin/stdout 传输，单条上限 16 MB。stdout 只能写协议消息，日志只能写 stderr。

使用 `agent_core::sdk::serve` 时，注册握手由 SDK 完成。宿主调用：

| 方法 | 参数 | 成功结果 |
|---|---|---|
| `agent.describe` | `{}` | descriptor 字段 |
| `agent.list` | `{}` | `{"sessions":[...],"warnings":[...]}` |
| `agent.load` | `{"id":"..."}` | `{"session":{...}}` |
| `agent.write` | `{"session":{...},"cwd":"...","overwrite":null}` | `{"target_id":"..."}` |
| `agent.delete` | `{"id":"..."}` | `{}` |
| `agent.destroy` | `{"id":"..."}` | `{}` |
| `agent.active` | `{"id":"..."}` | `{"active":true}` |
| `agent.backup_paths` | `{}` | `{"paths":["..."]}` |
| `agent.needs_rewrite` | `{"id":"..."}` | `{"needs_rewrite":false}` |
| `agent.prepare_open` | `{"id":"..."}` | `{}` |
| `agent.ensure_closable` | `{}` | `{}` |
| `agent.open_session_ids` | `{}` | `{"ids":["..."]}` 或 `{"ids":null}` |

错误返回使用 JSON-RPC error。单个请求失败不要让进程崩溃；返回 `Err` 即可。错误消息写中文，保留 anyhow 的上下文链。

## 6 错误与警告

- 整个数据库、目录或密钥不可用：从 `list`、`load` 或对应方法返回 `Err`。
- 只有单条会话无法解析：在 `list_with_warnings` 中跳过该条并写入 `AgentListing.warnings`，其余会话正常返回。
- `active` 和 `backup_paths` 查询失败必须返回错误，不能返回 `false` 或空数组冒充成功。
- 不要把 stderr 当日志上报接口；发布版 GUI 没有控制台。可读的错误必须走协议错误或 warnings。
- 插件内部的诊断、密钥、解析格式由插件负责，宿主只展示错误链，不解析具体 agent 数据。

## 7 测试

插件测试只启动自己的 exe，通过 JSON-RPC 验证协议和存储，不读取真实用户数据：

```rust
let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agent-plugin-example"))
    .env("EXAMPLE_DATA_DIR", temp_home)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .spawn()
    .unwrap();
```

至少覆盖：

- 发送 `plugin.register` 并接受
- `agent.list` 与 `agent.load`
- `agent.active`
- 未知方法返回 `-32601`
- 坏会话产生 warning 或整库错误
- 只读插件的写入错误
- 进程关闭后测试临时目录清理

仓库内运行：

```sh
build.cmd test-plugin <id>
build.cmd test-host
```

插件包测试不得依赖宿主 crate，也不得读写真实 agent 存储。

## 8 打包与导入

仓库内插件：

```sh
package-plugins.cmd <id>
```

输出：

```text
dist/plugins/<id>-<plugin-version>.zip
```

`package-plugins.cmd` 会：

1. 读取插件 `Cargo.toml` 的版本
2. 构建 `agent-plugin-<id>.exe`
3. 复制 exe、`icon.png`、可选 `data/`
4. 计算 exe SHA-256
5. 展开 `plugin.toml.in` 为 `plugin.toml`
6. 生成 zip

导入：

```sh
agent-session-manager.exe plugin import dist/plugins/<id>-<plugin-version>.zip
agent-session-manager.exe plugin list
```

插件改动后递增插件版本 patch，例如 `0.1.0` 到 `0.1.1`。已安装的同一版本目录不能覆盖导入，必须使用新版本或先卸载旧包。

## 9 发布前检查

- `cargo fmt --check` 通过。
- 插件测试通过。
- manifest 固定字段全部显式写出。
- `plugin.toml` 的 id、label、process、launch、version 与 `PluginDescriptor` 一致。
- exe 路径在包内，校验和与 manifest 一致。
- 图标为 512×512 的 PNG。
- 形态 id 唯一，最多一个 `default_open = true`。
- 只读插件的 `can_write`、`write`、`delete`、`destroy` 按实际实现一致。
- 版本检查的 `parser` 与 URL 匹配，路径或正则能解析出产品版本。
- 打包后的 zip 能导入，导入后插件页和 Agent 管理页都正常显示。
- 插件 README 已记录存储、写入、删除语义、限制和上游验证版本。

# 插件架构

插件是独立 Cargo 包，不是宿主里的远程适配器外壳。宿主只保留通用插件层，不链接任何插件 crate，也不包含 agent 特化适配器；全部 agent 能力在导入后的插件进程中运行。

从空目录写插件的步骤、完整模板和检查清单见 [plugin-development.md](plugin-development.md)。

## 1 边界

```text
agent-plugin-<id>.exe
  ├─ <产品> 存储能力            plugins/<id>-plugin/src/lib.rs
  ├─ 插件入口                   plugins/<id>-plugin/src/main.rs
  ├─ plugin.toml / icon.png     插件包自有资源
  └─ agent-core                 IR、JSON-RPC、短命进程入口、路径/时间/文本/JSONL/SQLite
```

- 宿主二进制不依赖 `plugins/` 下的任何 crate。
- 插件不依赖 `agent-session-manager`、Slint、UI、宿主 `assets/` 或宿主内部模块。
- 宿主不提供静态编译的内置 agent；所有 agent 都通过导入的插件包运行。
- 一个插件可以声明多个 variants，共用一个插件进程和同一份存储能力。存储或进程边界不同的产品才拆成两个插件。

宿主通用插件层固定为 `src/plugins/`：

```text
archive.rs    压缩包识别、安全解压与包根定位
manifest.rs   宿主侧 manifest、包内路径、SHA-256 与图标校验
registry.rs   插件注册表、跨进程锁与原子写入
process.rs    短命进程、超时、Job Object 与退出清理
adapter.rs    通用 ProcessPluginAdapter
discovery.rs  按注册表加载外部 agent
import.rs     staging、严格校验、探测、落盘与提交注册
```

这些文件不认识任何具体 agent。

## 2 包结构

```text
agent-core/            宿主与插件共用的 IR、协议、SDK 与公共工具
plugins/<id>-plugin/   独立插件包
```

发布包是一个 zip，包内必须有 `plugin.toml`、`icon.png` 与 `execution.command` 指向的 exe。插件私有文件统一放在包根 `data/`，随包原样安装；manifest 明确引用的形态图标可以放在包内其他相对路径。图标和 `data/` 不内嵌进 exe，宿主启动时从包目录读取。

`data/` 归插件所有：宿主不读取、不校验、不写入，插件进程以包根为工作目录自行使用。它不参与 SHA-256 校验，安装后被修改不会被发现，因此不能作为安全边界。插件版本升级会切换版本目录，其中的缓存应视为可重建。

源码包内的 `README.md` 记录该 agent 的存储位置、读写格式、删除语义、已知限制与上游验证版本。新增或修改插件只更新该 README，不在宿主文档汇总单 agent 细节。

每个插件包内部结构一致：

```text
plugins/<id>-plugin/
  Cargo.toml
  plugin.toml.in       manifest 模板，打包时注入版本与 SHA-256
  icon.png             512×512 图标
  data/                可选，插件私有数据，打包时原样复制
  src/lib.rs           存储能力，不实现宿主 AgentAdapter
  src/writer.rs        原生写入，按 agent 需要
  src/main.rs          PluginAgent + serve()
  tests/protocol.rs    真实子进程协议测试
```

宿主 `src` 不保留 `agents/<id>/` 包装目录。新增插件不修改宿主源码。

### 2.1 语言与交付格式

插件不强制使用 Rust 编写，本仓库的插件包只是 Rust 实现与示例。无论使用哪种语言，最终交付物必须是 Windows exe，并满足本文的 manifest、JSON-RPC 协议、校验与打包要求；宿主只启动 exe，不加载脚本或动态库。

选择非 Rust 实现的代价：

- 需要自行编译、打包 zip、维护版本与更新索引，本仓库不提供对应语言的构建脚本；
- 插件包不能作为 Rust crate 合并进本仓库或 Rust 插件库，只能通过导入的压缩包参与运行；
- 协议版本、IR 版本、超时、路径安全与错误语义需要自行保证，宿主不会为非 Rust 实现放宽校验。

## 3 依赖规则

- `agent-core` 不依赖宿主、UI 或插件包。
- 插件包只依赖 `agent-core`、标准库和自身需要的通用库。
- 插件测试只启动自身二进制走 JSON-RPC；宿主侧使用 fake 插件测试导入与适配器。
- 插件资源放在插件包内，不引用宿主图标源文件。
- 宿主通用插件层不得写入具体 agent id、agent 特化分支或试点测试夹具。
- 能力、插件入口和资源留在同一个插件包内，不为每个 agent 单独拆 adapter crate。

## 4 Manifest

插件包根必须有 `plugin.toml`，schema 固定为 2。下表顶层字段必须全部出现，即使值是 `false`、空数组或空字符串；完整可复制模板见 [plugin-development.md](plugin-development.md)。

| 字段 | 说明 |
|---|---|
| `schema` | 必须为 `2` |
| `id` | 小写 `[a-z][a-z0-9-]{1,31}` |
| `label` | 产品显示名 |
| `version` | 插件版本，安全文件名 |
| `protocol` | 插件协议版本，必须与宿主一致 |
| `ir_version` | IR 版本，必须与宿主一致 |
| `min_host` | 最低宿主版本，三段语义版本 |
| `process` | 用于进程检测的映像名 |
| `launch` | 默认启动目标，用于旧入口与显示 |
| `icon` | 相对包根的图标路径 |
| `can_write` | 是否可作为转接目标 |
| `sha256` | 插件 exe 的 SHA-256 |
| `group_color` | 产品色，用于界面分组 |
| `execution` | 进程模式、命令、参数与超时 |
| `capabilities` | 插件实现的协议能力 |
| `safety` | 排他、回滚与删除语义 |
| `integration` | 进程跟踪、打开会话与归档推断 |
| `variants` | 一个或多个产品形态 |

`execution.mode` 只接受 `ephemeral`，`command`、`args`、`timeout_ms` 必须写出，`timeout_ms` 位于 100 到 120000 之间。

`capabilities` 的 `list`、`load`、`write`、`delete`、`destroy`、`active`、`backup_paths` 与协议方法一一对应。`can_write = true` 时 `capabilities.write` 必须为 true。

`safety`：

| 字段 | 说明 |
|---|---|
| `requires_exclusive` | 写入前是否要求目标进程退出 |
| `restore_on_error` | 写入失败是否用整库备份回滚 |
| `delete_is_physical` | `delete` 是否本身就是物理删除，销毁与删除效果相同 |

`integration`：

| 字段 | 说明 |
|---|---|
| `tracks_process` | 是否有可探测的桌面进程 |
| `close_process` | 需要关闭的映像名；为空时关闭 `process` 指定的映像 |
| `close_mode` | `process` 或 `check_only` |
| `open_session_ids` | 是否支持读取客户端当前打开的会话 id |
| `archive_from_open_ids` | 客户端未打开的会话是否按已归档展示 |

### 4.1 形态

每个 `[[variants]]` 的 `id`、`label`、`kind`、`default_open`、`open` 必须写出。`icon`、`update`、`detectors` 按产品实际情况可选：

| 字段 | 说明 |
|---|---|
| `id` | 形态 id，小写，可含 `-`、`_`、`.` |
| `label` | 形态显示名 |
| `kind` | `desktop`、`cli` 或 `vsix` |
| `default_open` | 是否默认打开；一个插件最多一个 |
| `icon` | 相对包根的形态图标，可省略 |
| `open` | 打开策略 |
| `update` | 版本检查渠道，可省略 |
| `detectors` | 安装检测器列表，可省略 |

`open.strategy` 支持：

| 策略 | 行为 |
|---|---|
| `registry` | 通过注册表协议启动 |
| `command` | 执行 PATH 或绝对路径上的命令 |
| `candidates` | 依次尝试环境变量展开后的候选路径 |
| `vscode_workspace` | 打开工作区，再按 `session_uri` 打开会话 |
| `uri` | 调用 shell 打开 URI |

`open.command` 支持 `{cwd}` 与 `{session_id}` 占位；`session_args` 在带会话 id 时追加；`session_uri` 是工作区打开后的深链模板。

`update`：

| 字段 | 说明 |
|---|---|
| `channel` | 渠道 id，必须唯一 |
| `version_url` | Agent 产品版本检查页，抓取版本号用；不是插件包下载地址 |
| `parser` | 通用解析格式：`json`、`key`、`regex`、`html_regex` 或 `text` |
| `download_url` | Agent 产品下载页，给用户点开；不是插件包下载地址 |
| `paths` | `json` 使用的点分路径，按顺序尝试 |
| `key` | `key` 使用的顶层键名 |
| `patterns` | `regex`/`html_regex` 使用的正则，按顺序尝试 |
| `group` | 正则捕获组，默认 1；0 表示整个匹配 |
| `segments` | 只取前 N 段版本，0 表示不截取 |
| `min_parts` | 至少需要的数字段数，默认 2 |

宿主只实现上述通用格式，不按产品名分支。插件声明自己的路径或正则，新增 agent 不需要修改宿主 parser。旧 manifest 的 `url` 仍按 `version_url` 读取。

### 4.2 检测器

`variant.detectors` 的 `type` 支持：

| 类型 | 用途 |
|---|---|
| `desktop_app` | 卸载注册表、开始菜单快捷方式与常见安装目录 |
| `vscode_extension` | VS Code 与 VS Code CN 扩展目录 |
| `npm_package` | 全局 npm 包 |
| `native_binary` | 固定路径的二进制 |
| `command_version` | 执行命令读取版本，限时 5 秒 |

检测器字段按类型取用：`registry_keys`、`exclude_prefixes`、`exe`、`shortcut_names`、`state_db`、`roots`、`prefix`、`root`、`package`、`path`、`command`、`args`。未知类型在加载目录时跳过并写检查日志。

### 4.3 示例

```toml
schema = 2
id = "example"
label = "Example Agent"
version = "<plugin-version>"
protocol = 1
ir_version = 1
min_host = "<host-version>"
process = "example.exe"
launch = "example"
icon = "icon.png"
can_write = true
sha256 = "<exe-sha256>"
group_color = "#4a7bd0"

[execution]
mode = "ephemeral"
command = "bin/agent-plugin-example.exe"
args = []
timeout_ms = 30000

[capabilities]
list = true
load = true
write = true
delete = true
destroy = true
active = true
backup_paths = true

[safety]
requires_exclusive = true
restore_on_error = true
delete_is_physical = false

[integration]
tracks_process = true
close_mode = "process"
close_process = ""
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

`plugin.toml.in` 是打包模板。`package-plugins.cmd` 从插件自己的 Cargo 版本写入 `version`，从构建出的 exe 计算 `sha256`，并把模板展开为包内 `plugin.toml`。

## 5 版本与兼容

- 宿主与插件通过 `schema`、`protocol`、`ir_version` 与 `min_host` 判断兼容性；四项不匹配时拒绝加载。
- 插件版本独立于 app 版本，由插件包自行维护。
- 新增 agent、修改 IR 字段或转接格式时递增插件 minor；功能与修复递增 patch。
- 宿主升级后，旧插件只有在 `protocol`、`ir_version` 与 `min_host` 都满足时才会被加载。
- 插件运行时不检查版本号大小，只做精确协议匹配；上游 schema 变化以请求错误返回，不静默写入。

## 6 校验

启动发现与导入校验刻意不同：

启动 `PluginManifest::load`：

- 解析 manifest，校验 schema、protocol、IR、`min_host`、id/version 格式与执行模式；
- 要求 `plugin.toml` 可读、exe 存在、icon 存在、路径不逃逸；
- 不计算 exe 整文件 SHA-256，不解码图标，避免启动时扫描大文件；
- 每次注册握手再次比对 exe SHA-256。

导入 `PluginManifest::load_strict`：

- 包含启动校验的全部项目；
- 计算 exe SHA-256，必须与 manifest 一致；
- 解码 icon，必须为 512×512；
- 通过后进入探测握手与注册表提交。

## 7 导入

只接受压缩包，不接受目录。压缩包可以带单一顶层目录，但 `plugin.toml` 必须位于包根或该唯一顶层目录。支持：

```text
.zip
.tar
.tar.gz / .tgz
.tar.xz / .txz
.tar.zst / .tzst
.tar.bz2 / .tbz2 / .tbz
.7z
```

流程：

```text
选择压缩包
→ 解压到 plugins/.staging/<uuid>
→ 安全路径与条目校验
→ 定位 plugin.toml 所在包根
→ manifest 严格校验：路径、SHA-256、512×512 图标
→ 原子移动到 plugins/<id>/<version>
→ 探测模式启动插件并完成 plugin.register
→ 校验 runtime 描述与 manifest
→ app.exe 写入 plugin-registry.json
→ 返回可发现的 agent
```

- 同一 id 已登记其他版本时，导入流程显式激活新版本；registry 一个 id 只保留一个条目。
- 目标 `<id>/<version>` 已存在时拒绝导入。
- 探测失败或注册失败时删除刚落盘的目录，不留下无效登记。
- 导入即解压到数据目录；运行时不读取 zip，始终启动已解压目录中的 exe。
- 包内 `data/` 原样安装；宿主不解析、不校验、不哈希其中的内容。

解压安全边界：

- 拒绝绝对路径、盘符、UNC、`..` 与空路径；
- 拒绝符号链接、硬链接、7z 删除标记与重解析点属性；
- 拒绝重复路径，Windows 下按大小写不敏感判重；
- 拒绝 Windows 保留名、结尾空格或点、`:` 数据流与控制字符；
- 条目数上限 4096，单文件解压上限 128 MB，总解压量上限 512 MB，压缩包本身上限 512 MB；
- 失败时删除整个 staging，不留下半成品。

## 8 协议

JSON-RPC 2.0，一行一条 UTF-8 JSON，stdin/stdout 传输，stderr 仅日志，单条上限 16 MB。

宿主调用：

- `agent.describe`
- `agent.list`
- `agent.load`
- `agent.write`
- `agent.delete`
- `agent.destroy`
- `agent.active`
- `agent.backup_paths`
- `agent.needs_rewrite`
- `agent.prepare_open`
- `agent.ensure_closable`
- `agent.open_session_ids`

插件启动后先发送 `plugin.register` 申请。宿主校验注册申请与已安装 manifest 一致，再接受业务请求。错误码为整数，机器可读类型放 `error.data.kind`。未知方法返回 `-32601`，参数错误返回 `-32602`，内部错误返回 `-32603`。

`agent.list` 成功结果包含 `sessions`，可选包含 `warnings`：

```json
{
  "sessions": [],
  "warnings": [
    {
      "kind": "parse_skipped",
      "message": "会话 abc 无法解析：字段缺失",
      "session_id": "abc"
    }
  ]
}
```

`warnings` 用于单条会话无法解析但其余会话仍可读取的情况；插件应跳过该会话并继续。整个数据库或密钥不可用时仍返回错误。`kind` 由插件定义，宿主只展示 `message`，不按类别分支。

错误消息会保留插件的 anyhow 上下文链；插件应把用户能看懂的原因放在最外层消息或 context 中。

## 9 运行时数据流

```text
宿主启动
→ 读取 plugin-registry.json
→ 逐个启用插件加载 manifest 与 icon
→ 创建 AgentInfo 与 ProcessPluginAdapter
→ 注册到运行时 agent 列表
→ UI、目录、安装扫描、版本源、打开与转接全部读取该列表
```

- 产品目录由已安装插件的 variants 组装，包含名称、形态、检测器、产品色、默认打开与渠道 id。宿主不保留静态产品表。
- 安装扫描按 variants 的 detectors 逐形态执行。
- 版本检查的默认渠道来自 variants 的 update 声明；数据目录 `update-sources.json` 按渠道覆盖 `version_url`、`download_url` 与解析参数。
- 打开与默认打开形态来自 variants 的 open 与 default_open。
- 会话列表读取失败或插件跳过单条会话时，会话页顶部提示条显示摘要，可点开可滚动的错误详情；完整错误链来自插件的 JSON-RPC 错误或 `warnings`。
- 插件页读取 registry 与运行时 agent 列表，显示名称、版本、形态数、能力、启用开关与卸载入口。
- 停用插件只把该 id 移出运行时注册表，包与 registry 记录保留；卸载删除插件包目录与 registry 记录，不删除 agent 会话数据。
- 导入、启用、停用或卸载成功后立即重建运行时注册表、产品目录、安装扫描与版本源。
- 插件图标从包目录读取；运行时缓存没有命中时按插件 id 回退到已导入包的 `icon.png`。
- 数据目录由 `config.json` 的 `plugins.root` 决定；registry 保存相对于该根目录的路径。

## 10 生命周期

默认短命模式：

```text
spawn → plugin.register → 宿主接受 → 请求 → 响应 → 关闭 stdin → 退出
```

- 宿主每次操作启动一个新进程，空闲时没有插件进程。
- 单个请求超时后整树终止。
- 子进程加入 Windows Job Object，宿主异常退出时由 `KILL_ON_JOB_CLOSE` 兜底。
- 宿主收到退出请求后先拒绝新任务，再关闭 stdin、等待 2 秒，最后终止残留进程。
- 生命周期读写锁保证子进程登记与宿主 shutdown 不会漏掉竞态窗口。
- 清理只按本次创建的 PID 操作，不按映像名结束用户 agent。

## 11 注册表与发现

- registry 只由 app.exe 写入。
- 进程内与跨进程写者都串行化，Windows 使用命名 mutex。
- registry 使用临时文件加原子替换，每次真实变更递增 `generation`。
- 相同 id、版本、哈希与根路径的运行时申请幂等。
- 运行中的插件不能静默替换不同版本；升级只通过导入流程激活。
- 同一 id 只能登记一个插件。
- 启动发现只记录坏插件警告，不阻断应用。
- registry 不可读时启动发现记录警告并跳过插件，不阻断应用启动。

## 12 宿主安全职责

插件以当前用户权限运行，不是沙箱。

宿主保留：

- 写入与删除前的目标进程检查；
- `agent.backup_paths` 结果校验、复制到 app 数据目录与整库备份；
- 写入失败后的回滚；
- 删除与销毁的界面二次确认；
- registry 写入与插件进程回收。

插件只按运行时环境报告需要备份的路径，不能指定备份目的地，也不能参与复制或回滚实现。

## 13 构建、测试与打包

```sh
build.cmd host          # 只编 app.exe
build.cmd plugins       # 只编全部插件
build.cmd plugin <id>   # 只编指定插件
build.cmd all           # 本体 + 插件
build.cmd test-host
build.cmd test-plugins
build.cmd test-all
```

等价参数：

```sh
cargo build --release -p agent-session-manager
cargo build --release --workspace --exclude agent-session-manager
cargo build --release -p agent-plugin-<id>
cargo build --release --workspace
```

`cargo build` 默认只构建本体；插件构建不会触发宿主 `build.rs` 的 Slint 与图标编译。

插件测试分为两层：插件包内测试启动自身 exe，验证注册与协议；宿主测试构造 fake 插件包与 fake 协议进程，验证导入、发现与适配器调用。

生成可导入插件包：

```sh
package-plugins.cmd          # 全部插件
package-plugins.cmd <id>     # 只构建并打包指定插件
```

输出：

```text
dist/plugins/<id>-<plugin-version>.zip
```

`package-plugins.cmd` 会把插件源码目录下的 `data/` 原样并入发布包；没有该目录时不生成。

导入与查看：

```sh
agent-session-manager.exe plugin import dist/plugins/<id>-<plugin-version>.zip
agent-session-manager.exe plugin list
```

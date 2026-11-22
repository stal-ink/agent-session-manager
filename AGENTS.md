# AGENTS.md

本文件是本仓库的工作规范，面向所有在此修改代码、图标与文档的 agent。README 描述用户功能，本文件描述实现约束；两者冲突时以本文件为准。

## 项目边界

- Windows 桌面工具，Rust 2021 + Slint 1.19。默认 Cargo feature `renderer-skia` 使用 Skia 渲染器；`cargo build --no-default-features` 使用 Slint 自带的 femtovg，不依赖 Skia 预编译库。跨屏 DPI 仍由 `src/ui/dpi.rs` 的窗口过程钩子处理。
- 读取已安装插件声明的本地 agent 会话，经 IR 转成任一支持写入的 agent 原生格式，并支持 Markdown 双导出（普通文档与交接文档）。
- 插件是独立 Cargo 包，不是宿主内包装器。宿主通用插件层不得写死具体 agent id。共享基础保持为一个 `agent-core` crate，不再为每个 agent 拆 adapter crate。
- 仓库固定在当前路径，不迁移、不重建、不复制到其他目录。
- 源端存储只读。除原生转接的目标写入外，不修改任何 agent 的真实数据。显式例外只有「销毁」：界面批量操作发起的物理删除，必须满足勾选确认、目标进程关闭、整库备份、逐 agent 事务四条（见 [docs/safety.md](docs/safety.md)），测试与脚本一律不得调用。

## 硬性要求

### 图标

- 界面单色图标统一使用 Lucide 1.75.0，24px 网格、2px 描边、ISC 协议。图标在 https://lucide.dev/icons/ 查找，从 npm 包 `lucide-static` 下载；本机经 npmmirror 取 `https://registry.npmmirror.com/lucide-static/-/lucide-static-1.75.0.tgz`，解压后把 `package/icons/<name>.svg` 复制到 `assets/icons/`。保留 SVG 的 `currentColor`，由 Slint `Image.colorize` 按主题令牌上色。单色图标不再手绘；品牌图标与双 A 字标不受此限。
- 所有 agent 图标源文件不低于 512×512。PNG 统一放在 `assets/agents/512x512_png/`，SVG 源文件统一保留在 `assets/agents/svg/`，不得再散放到 `assets/agents/` 根目录。
- 新图标必须来自官方安装目录的高分辨率原图，用 Lanczos 缩放到 512×512，不放大低分辨率图。
- 各插件图标保持官方样式变体与品牌色，不统一改色。
- 新增 agent 时在插件包内提供 `icon.png`（512×512），宿主不按 id 加载图标；`assets/agents/512x512_png/` 保留图标源文件，不参与宿主注册或构建。
- 应用图标 `assets/app.ico` 不低于 256×256。修改图标后必须重新构建发布版。

### 版本

- 版本号由执行改动的 agent 自主维护，不需要用户提醒。
- 应用版本写在根 `Cargo.toml`，`Cargo.lock` 由构建更新，两者必须一致。插件包版本独立于 app 版本，由各插件包维护；兼容性由 `protocol`、`ir_version`、`min_host` 判断。`agent-core` 使用独立语义版本。
- 现有 agent 的功能、修复、图标变更递增 patch，例如 0.6.2 → 0.6.3。
- 新增 agent、修改 IR 字段或转接格式递增 minor。
- 纯文档、测试、格式化提交可不递增版本。
- 版本变更与代码放在同一提交；app 发布产物名使用 app 版本：`agent-session-manager-v<version>-win-x64.exe`，zip 同名；插件包使用插件版本：`<id>-<plugin-version>.zip`。
- 各插件的上游验证版本记录在插件包 `README.md`，修改对应适配器后同步更新。

### 插件包

- 发布包是 zip，包内必须有 `plugin.toml`、`icon.png` 和被 manifest `execution.command` 指向的 exe。插件私有文件统一放在包根 `data/`，打包时原样复制；manifest 明确引用的形态图标可以放在包内其他相对路径。图标不内嵌进 exe；宿主在启动时从包目录读取。`data/` 归插件所有，宿主不读取、不校验、不写入，也不作为安全边界。
- manifest schema 2：插件声明 `variants`（id/label/kind/default_open/open/update/detectors）与 `group_color`；启动策略、版本地址、安装检测都来自插件，宿主不再按 agent id 写分支。一个新插件可以声明多个形态，但共用一个插件进程和同一份存储能力；存储/进程边界不同的产品才拆插件。
- 产品目录与安装扫描以已安装插件的 manifest 为数据源，宿主不保留静态产品表；版本检查默认源同样来自插件。
- 只接受压缩包导入，不接受目录；支持 zip、tar 系压缩与 7z。压缩包可以带单一顶层目录，但 manifest 必须位于包根或该唯一顶层目录。
- 解压必须拒绝路径逃逸、符号链接与重解析点、重复路径、Windows 保留名等不安全条目，并限制条目数、单文件解压大小和总解压量。
- 启动发现只做 manifest 结构、包内路径和 exe/icon/toml 存在性检查，不解析 `data/` 内容；导入才做 SHA-256 与 512×512 图标解码；每次注册握手重新比对 exe SHA-256。不得把启动检查退化成“目录里有什么就信什么”。
- 插件包只依赖 `agent-core` 与自身源码；normal/build/dev 依赖不得包含宿主、Slint、UI 或宿主 `assets/`。宿主二进制不得依赖任何插件 crate；插件只通过导入后的数据目录参与运行。

## 架构约束

- `AgentAdapter` 是宿主侧契约，只由通用 `ProcessPluginAdapter` 实现；插件包不得实现宿主适配器，也不得自报安全语义。插件通过 JSON-RPC 提供 list/load/write/delete/destroy/active/backup_paths 等能力，宿主按 manifest 声明、注册握手 SHA-256 与逐能力检查校验每次调用。新增插件必须补齐对应协议方法；`destroy` 未实现时由宿主按 `delete` 处理，软删除插件必须在插件内提供物理销毁。
- 备份与回滚只由 app 执行：插件按运行时环境返回 `backup_paths`，宿主负责校验、复制到 app 数据目录、失败回滚；插件不得提供备份目的地或回滚实现。删除/销毁的二次确认、进程闸门同样在宿主。
- IR 与插件协议定义在 `agent-core/`，宿主与插件共用。各 agent 的存储实现都位于各自 `plugins/<id>-plugin` 包内；宿主 `src` 不得出现具体 agent id 的存储/UI 分支、`src/agents/<id>` 包装目录或插件 crate 引用。新增插件只新增插件包并导入 zip，不得要求修改 app.exe 源码。版本检查规则由 manifest 声明，宿主只实现 json/key/regex/html_regex/text 等通用格式，不新增产品专用 parser。插件包自带 manifest 模板和图标，不引用 `assets/agents/`。
- IR 只保留跨 agent 的公共语义。agent 私有字段不进 IR，读取时丢弃或降级为文本；不恢复 `Product` 枚举，不在公共结构里堆单一产品的字段。
- 原生写入必须满足三条：目标进程完全关闭、写前整库备份、写入使用事务。任一条不满足就拒绝写入。插件可在 manifest 的 `safety` 声明不排他或失败不回滚，此时由插件自行协调共享状态并清理半成品；会话锁等额外约束由插件实现。外部插件的 `active` 与 `backup_paths` 查询失败必须保留错误，不得把 IPC 故障当成“不活跃”或“空备份范围”。
- 插件进程是短命进程：宿主每次请求启动、完成即退出；daemon 暂不实现。插件启动必须先发送 `plugin.register` 申请，宿主与已安装包一致才接受；`plugin-registry.json` 由 app.exe 串行、原子写入。宿主退出前必须先等待或终止本进程启动的所有插件，Windows 下同时用 Job Object（KILL_ON_JOB_CLOSE）兜底；插件清理只允许按本次启动的 PID 管理，不得按映像名结束用户 agent。
- 备份包含数据库与 `-wal`、`-shm`；正文不在数据库里的 agent 还要备份正文目录。只保留最近 1 份。回滚步骤写在 [docs/safety.md](docs/safety.md)。
- 转接台账记录 `source_updated_ms`：命中且源未更新且目标活跃时返回已转接，否则以原目标 id 覆盖。
- 会话状态固定为四态：活跃中、已归档、工作区归档/删除、已删除。标签优先级红、灰、黄、绿；排序按四态分块，块内按工作区聚合，不跨状态交错。
- 读取顺序、压缩边界、写入格式与状态判定由各插件实现，并记录在对应插件包 `README.md`。修改插件读写器时同步更新该 README，不在宿主文档汇总单 agent 细节。
- 界面文案以需求文本为准逐字实现，不自行润色。界面与错误信息使用中文。
- 版本检查只在后台线程发 GET 请求，`ureq` + rustls，单次超时 8 秒；由「自动检查更新」设置（默认开启）在启动后延迟 5 秒检查一次，之后每 20 分钟检查一次；设置关闭时不自动检查，只保留手动触发。
- 检查单线程依次进行，覆盖已安装形态与未安装条目；没有检查地址的渠道不发请求；失败保留上次成功版本并记录失败状态。检查结果按渠道缓存 `updates.json`，只含结果与时间；日志只在内存实时更新，保留最近 20 条，行内不显示失败标记。测试不联网，解析函数只吃字符串。
- 版本源默认值来自已安装插件 manifest；数据目录 `update-sources.json` 只覆盖同渠道的 Agent 产品版本检查页 `version_url`、`parser`、Agent 产品下载页 `download_url` 与解析参数，未在插件中声明的旧渠道不参与检查。`version_url` 与 `download_url` 都不是插件包地址；插件包地址只出现在 `plugins-index.json` 的 `zip_url`。解析规则必须是通用格式加路径/正则，不允许宿主出现具体产品 parser；`installs.json` 只是安装扫描缓存。
- 应用配置只写 `%LOCALAPPDATA%/agent-session-manager/config.json`，不写 agent 存储；`updates.json` 只存版本检查结果。
- CLI `doctor` 只读；`transfer` 与界面共用同一套校验、备份和写入逻辑；`plugin import <archive>` 与 `plugin list` 是插件入口，导入行为遵循「插件包」一节的规则。registry 只能由 app.exe 原子写入。发布版保持 GUI 子系统，CLI 子命令通过 `AttachConsole` 挂接父控制台。

## 开发流程

- 构建与检查必须区分对象：`build.cmd host`/`cargo build --release -p agent-session-manager` 只编本体；`build.cmd plugins` 或 `build.cmd plugin <id>` 只编插件；`build.cmd all`/`--workspace` 才同时编。`cargo build` 受 `default-members = ["."]` 控制，默认只编本体，不得让普通宿主构建触发插件编译。测试同理：`build.cmd test-host`、`test-plugin <id>`、`test-all`。
- Skia 构建：默认 feature `renderer-skia` 启用 `slint/renderer-skia`，运行时 `renderer_name("skia")`；`--no-default-features` 时运行时使用 `femtovg`，不读取 Skia 库。
- `skia-bindings` 0.157.0 的官方预编译文件以解压形式固定存放于 `third_party/skia/`（两个 `.lib`、`bindings.rs`、`tag.txt`、`key.txt` 与许可证），不入库。本地 `third_party/skia-bindings` 增加 `SKIA_BINARIES_DIR` 覆盖入口，未设置时构建脚本自动取本 crate 同级的 `third_party/skia`，并校验目录内 tag/key。库已就位时 Skia 构建不依赖网络；`build.cmd host/all/test-host/test-all` 与 `package.cmd` 在库缺失时调用 `fetch-skia.ps1` 下载并校验，直接 `cargo build`/`cargo run` 不自动下载。
- `bindgen` 0.73.2 需配合 `prettyplease` 0.3.0，`Cargo.lock` 已固定；升级依赖或变更 Skia 特性时必须同步更换匹配的文件与本地适配并复核。
- 构建依赖的源码、安装包与预编译库只能放在仓库的专用依赖目录（如 `third_party/`）；禁止放在系统或用户临时目录、用户目录（含 Desktop、Downloads、Documents、Home）及 `target/`。Cargo/rustc 正常生成的中间产物可以位于 `target/`，但不得把 `target/` 当作依赖来源或在其中手工补库。
- 测试使用 `tests/common` 的临时目录与同构数据库，不读取、不写入真实 agent 存储。插件包测试位于各自 `plugins/<id>/tests/`，只验证插件自身协议与存储，不得依赖宿主 crate；宿主通用插件测试不得写死具体 agent，需要适配器集成测试时构造 fake 插件。`agent-core` 公开项必须有 rustdoc，改动后运行 `build.cmd check-docs`。
- CI（GitHub Actions，`.github/workflows/ci.yml`）在每次 push 与 PR 上执行格式检查、agent-core 文档检查与全量测试；Skia 预编译库由 `fetch-skia.ps1` 在 CI 上自动下载。
- 读写器改动必须补测试。新增行为覆盖成功路径、覆盖路径与异常路径。
- 真实存储验证需要用户明确同意。验证前备份，验证后运行 `PRAGMA quick_check`，确认备份可回滚。
- 修改存储格式或用户可见行为时同步更新 README 与对应文档；单 agent 的存储、写入、限制与验证版本写入插件包 `README.md`。
- 发布构建的安全加固必须保留（见 `docs/security.md`）：`.cargo/config.local.toml` 的 `--remap-path-prefix` 映射（不入库，模板为 `.cargo/config.local.example.toml`，构建机或仓库路径变更时同步更新，`RUSTFLAGS` 环境变量会整体覆盖 rustflags）与 `main` 的 panic hook（release 不输出文件路径与行号）。发版前按该文档判据抽查产物：符号剥离、绝对构建路径与 CodeView/RSDS 引用为零。PDB 永不入 `dist/`。
- docs 目录约定：活跃文档直接放在 `docs/` 根，文件名使用小写 kebab-case，一主题一文；README 只保留概览与文档索引。宿主文档只写通用架构、插件机制、功能、数据目录、安全、加固、限制与交接导出；各 agent 的存储、写入、删除语义、限制与验证版本写在 `plugins/<id>-plugin/README.md`。已完成的方案、调研、缺陷报告与历史评估直接删除，不建 archive 或 research 目录，git 历史保留。
- 文档文风（适用于 README、AGENTS、docs 与插件包 README）：短句直说；禁元话语（「值得注意的是」「需要说明的是」「总的来说」「简单来说」「综上所述」「换句话说」「如下所示」「本文档将介绍」）；禁作者视角叙事（「本机 CLI」「本机已完成验证」「真机验证」「装机版」——验证结论写成陈述当前能力的现在时，版本事实保留「验证版本 X」）；禁变更日志语态（「已改为」「替代原来的」「本次是特殊情况」）与「已被取代」「仅供参考」横幅；禁面向维护者的操作指令——密钥保管、发布步骤等发布端内容不进入公开文档，公开文档只描述客户端行为与信任模型；同一事实只在一处完整陈述，同一概念全文一个名字；装饰字符与情绪评价词不入正文；提交前核对文档间链接指向的文件存在。
- 发布打包只放运行时文件：exe 与同名 zip，输出到 `dist/`，该目录不入库。插件包由 `package-plugins.cmd [id...]` 输出为 `dist/plugins/<id>-<plugin-version>.zip`；不传 id 才处理全部，只改一个插件时只构建/打包该插件，与宿主包分开。打包脚本不得调用 `del`、`rmdir`、`Remove-Item` 或任何回收站 API，也不建暂存目录再清理；zip 直接以覆盖模式生成。
- UI 必须有独立「插件」页：提供导入按钮和已导入插件列表；Agent 管理页的产品/形态行来自插件 manifest，不得再显示宿主静态产品表。
- Commit messages follow Conventional Commits in English (`feat:`, `fix:`, `docs:`, `refactor:`, `init:`); body as bullets, one item per bullet. 界面文案与文档仍为中文。
- 只有用户明确要求时才提交，不自动推送远端。
- 修改源码使用编辑器工具，不写 sed、node、python 批量替换脚本。图片处理脚本不受此限。
- 删除用户会话只能由界面的批量删除操作触发，必须二次确认、写入前检查目标进程、写前备份并支持失败回滚。销毁（物理删除）同样只能由界面批量操作触发，额外要求勾选「我已知晓不可恢复」；不得在测试、脚本或自动流程中删除或销毁真实会话。
- 不清理用户存储、不结束用户正在使用的 agent 进程；只能操作本次任务自己启动的进程。

## 提交前检查

- `cargo fmt --check` 通过。
- `agent-core` 改动后运行 `build.cmd check-docs`，missing docs 与 rustdoc 链接检查通过。
- 对应范围的测试通过；改动共享代码或插件契约时运行 `build.cmd test-all`。
- 图标尺寸不小于 256×256。
- `Cargo.toml`、`Cargo.lock`、发布产物版本一致。
- README 与架构文档没有与新行为冲突的描述。
- 没有把真实用户数据、备份、导出或 `dist/` 加入暂存区。

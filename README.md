# Agent Session Manager

**本地 AI Agent 会话管理与转接工具**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/Platform-Windows-0078D6.svg)
[![Rust 2021](https://img.shields.io/badge/Rust-2021-orange.svg)](https://www.rust-lang.org/)
[![Slint 1.19](https://img.shields.io/badge/UI-Slint%201.19-2379F4.svg)](https://slint.dev/)

**平台限制：仅支持 Windows 10/11（x64），其他平台不可用。**

读取本地 AI agent 会话，经统一 IR 转接：写入目标 agent 原生存储以继续对话，或导出 Markdown 上下文。源端只读，不上传会话内容，无遥测。

## Agent 支持

Agent 支持由导入的插件包决定，不写死在宿主。仓库在 `plugins/` 下提供插件包，每个包自带 `README.md`，说明该 agent 的存储位置、读写方式、删除语义与已知限制。新增产品只需新增插件包并导入，不需要修改宿主。

已安装插件列表与插件包路径可通过 `agent-session-manager.exe plugin list` 查看。

## 特性

- 统一会话列表：合并已安装插件提供的会话，按状态与工作区聚合，支持过滤与搜索。
- 原生转接：写入目标存储，会话出现在目标应用列表并可直接续聊。
- 双导出：普通文档全量保留；交接文档按固定规则裁剪重排，离线生成。
- 批量操作：导出、删除、销毁，写前备份，失败回滚。
- Agent 管理：多形态安装检测、版本检查与默认打开形态。
- 插件化：agent 支持以独立插件包分发，插件只从压缩包导入，运行形态是解压后的目录。
- 本地优先：不发送遥测，版本检查只发匿名 GET。

完整交互行为见 [docs/features.md](docs/features.md)。

## 快速开始

Windows 平台。依赖 Slint 1.19 与内置 SQLite。

```sh
cargo run
```

发布构建与打包：

```sh
package.cmd
```

在 `dist/` 生成 `agent-session-manager-v<version>-win-x64.exe` 与同名 zip。加 `--skip-tests` 跳过测试。

只读诊断与命令行转接：

```sh
agent-session-manager.exe doctor
agent-session-manager.exe transfer <source-agent> <session-id> [--to <agent>] [--export] [--cwd <path>]
```

`<source-agent>` 与 `<target-agent>` 使用已安装插件声明的 id。

插件包构建与导入：

```sh
package-plugins.cmd
agent-session-manager.exe plugin import dist/plugins/<id>-<plugin-version>.zip
```

界面侧边栏「插件」页也提供导入入口。从空目录开始写插件的完整步骤见 [docs/plugin-development.md](docs/plugin-development.md)，宿主机理见 [docs/plugin-architecture.md](docs/plugin-architecture.md)。

## 文档

| 文档 | 内容 |
|---|---|
| [architecture.md](docs/architecture.md) | IR、适配器契约、转接与导出流程 |
| [plugin-development.md](docs/plugin-development.md) | 从空目录到可导入插件包的完整开发步骤 |
| [plugin-architecture.md](docs/plugin-architecture.md) | 插件包、manifest、导入、协议与运行时 |
| [features.md](docs/features.md) | 界面与交互行为 |
| [storage.md](docs/storage.md) | 本工具数据目录与备份约定 |
| [safety.md](docs/safety.md) | 备份、回滚、删除与销毁 |
| [security.md](docs/security.md) | 二进制加固与发布前检查 |
| [limitations.md](docs/limitations.md) | 宿主与插件机制的已知限制 |
| [handoff-export.md](docs/handoff-export.md) | 交接文档裁剪规则 |
| [updates.md](docs/updates.md) | App 自更新与插件在线更新的清单、签名、下载与安装 |

各插件文档位于 `plugins/<id>-plugin/README.md`。

## 数据与隐私

本工具数据位于 `%LOCALAPPDATA%/agent-session-manager/`。原生写入前检测目标进程并整库备份，删除与销毁需二次确认且销毁不可恢复。路径见 [docs/storage.md](docs/storage.md)，安全约束见 [docs/safety.md](docs/safety.md)。

## 开发

```sh
build.cmd host          # 只编译 app.exe（Skia 渲染器）
build.cmd host-femtovg  # 只编译 femtovg 版本，不需要 Skia 预编译库
build.cmd plugins       # 只编译全部插件
build.cmd all           # 本体与全部插件
build.cmd test-host
build.cmd test-all
build.cmd check-docs    # 检查 agent-core 文档与链接
```

`cargo build` 默认只构建宿主，并使用 Skia 渲染器；Skia 预编译库固定在 `third_party/skia/`，构建时由 skia-bindings 本地适配自动定位。首次运行 `build.cmd host`（或 `all`、`test-host`、`test-all`）或 `package.cmd` 时，脚本会自动下载并校验预编译库；直接 `cargo build`/`cargo run` 不会自动下载。网络受限时可设置环境变量 `SKIA_BINARIES_URL` 指向镜像地址。没有该目录或想避开 Skia 时，用 `cargo build --no-default-features`（或 `build.cmd host-femtovg`）构建 femtovg 版本。发布加固的路径映射放在 `.cargo/config.local.toml`，从 `config.local.example.toml` 复制后按本机路径填写。插件打包可只处理指定包：

```sh
package-plugins.cmd <id>
```

工程结构与模块边界见 [docs/architecture.md](docs/architecture.md)。

## 发布

本仓库提供构建与打包脚本：

| 路径 | 用途 |
|---|---|
| `package.cmd` | 宿主 exe 与 zip 打包 |
| `package-plugins.cmd` | 插件包打包，可只传指定插件 id |

签名与威胁模型见 [docs/security.md](docs/security.md)，更新系统的数据契约与流程见 [docs/updates.md](docs/updates.md)。未设置 `ASM_UPDATE_PUBKEY` 构建的 exe，更新检查会提示未配置签名公钥。

## 许可证

MIT，详见 [LICENSE](LICENSE)。

# 更新系统

App 自更新与插件在线更新的数据契约、地址布局与更新流程。用户可见行为见 [features.md](features.md)，签名与威胁模型见 [security.md](security.md)。

## 目标

- App 自更新：读取远端清单 → 下载 exe → 验签 + SHA-256 → 退出后由 helper 替换 → 重启；失败可回滚。
- 插件在线更新：读取插件索引 → 下载 zip → 验签 + SHA-256 → 走现有严格导入 → 刷新能力。
- 更新动作由用户发起：App 安装前有确认对话框，插件更新由插件页下载按钮触发；不静默替换、不修改任何 agent 会话数据。

## 总体架构

```text
GitHub Releases / 固定更新地址
  app.json + app.json.sig
  plugins-index.json + plugins-index.json.sig
  agent-session-manager-vX.exe
  <plugin>-<version>.zip
          │ HTTPS
          ▼
宿主
  检查 → 验签清单 → 比版本 → 下载 → 校验 SHA-256
    ├─ App：helper 退出后替换 exe，失败回滚
    └─ 插件：严格导入 → 激活新版本 → 刷新能力与 UI
```

信任链：app 内置 Ed25519 公钥 → 验签清单 → 信任清单中的 URL 与 SHA-256 → 下载 → 校验 → 安装。

## 数据契约

### app.json

```json
{
  "schema": 1,
  "version": "0.18.0",
  "exe_url": "https://github.com/<owner>/<repo>/releases/download/v0.18.0/agent-session-manager-v0.18.0-win-x64.exe",
  "sha256": "……",
  "size": 35840000,
  "notes_url": "https://github.com/<owner>/<repo>/releases/tag/v0.18.0"
}
```

- `sha256` 是下载 exe 的哈希，不是本地文件哈希。
- `size` 用于进度与磁盘检查。
- 拒绝等于或低于当前版本的清单，避免降级和更新循环。

### plugins-index.json

```json
{
  "schema": 1,
  "generated_at": "2026-11-21T00:00:00Z",
  "plugins": [
    {
      "id": "trae",
      "label": "Trae CN",
      "version": "0.1.0",
      "zip_url": "trae-0.1.0.zip",
      "sha256": "……",
      "size": 1329001,
      "min_host": "0.17.0"
    }
  ]
}
```

- `sha256` 是整个插件 zip 的哈希，不是包内 exe 的哈希。
- `zip_url` 是插件包 zip 的地址，优先写相对文件名，由宿主按索引所在目录展开；也接受绝对 HTTPS 地址。相对地址让线上索引与本地测试服务器共用同一份文件。
- 旧索引里的 `url` 字段仍按 `zip_url` 读取，便于已生成的索引继续使用。
- Agent 产品自己的更新地址仍在插件包 `plugin.toml` 的 `[variants.update]`，索引不重复保存。
- `label` 来自插件 zip 根部的 `plugin.toml`，由生成索引的工具读取，用于未安装列表展示；缺失时回退到 `id`。
- `min_host` 不满足时提示先升级 app，不下载。

四类地址不要混用：

| 地址 | 所在文件 | 字段 | 含义 |
|---|---|---|---|
| 软件本体 exe | `app.json` | `exe_url` | App 自更新下载的 exe |
| 插件包 zip | `plugins-index.json` | `zip_url` | 插件在线更新/安装的 zip |
| Agent 产品版本页 | `plugin.toml` `[variants.update]` | `version_url` | 抓取版本号用的页面 |
| Agent 产品下载页 | `plugin.toml` `[variants.update]` | `download_url` | 给用户打开的下载页 |

### 签名

- 清单对原始字节做 Ed25519 签名，生成 `app.json.sig`、`plugins-index.json.sig`。
- 签名文件为 64 字节签名的十六进制文本，与清单放在同一目录，可自行核验。
- App 先验签，再解析 JSON；验签失败直接报错，不下载。
- 客户端内置单一公钥；更换公钥需要一次由旧公钥签名的 app 更新过渡。

## 地址布局

仓库为 `https://github.com/stal-ink/agent-session-manager`，采用两条独立地址：

```text
App 清单：
https://github.com/stal-ink/agent-session-manager/releases/latest/download/app.json

插件清单（固定标签 plugins，标记为预发布）：
https://github.com/stal-ink/agent-session-manager/releases/download/plugins/plugins-index.json

插件 zip：
https://github.com/stal-ink/agent-session-manager/releases/download/plugins/<id>-<version>.zip
```

- app 资产跟随版本 tag，`latest/download` 永远指向最新正式 release。
- 插件使用固定 `plugins` 预发布标签，通过 `--clobber` 替换资产；预发布不会成为 `releases/latest`。
- 宿主在 `src/update/mod.rs` 维护 `REPO_ROOT`、`PLUGIN_TAG`，并派生 `app_release_base()`、`plugin_release_base()`、`app_manifest_url()`、`plugin_index_url()`；调用处不写仓库地址。
- 换镜像或本地测试服务器时索引内容不用改；只有 app 清单的 `exe_url` 保持绝对地址。

## 宿主实现

```text
src/update/
  mod.rs        公共：清单解析、SHA-256、签名验证、流式下载与进度
  app.rs        App 下载路径
  plugins.rs    插件 zip 下载路径
  install.rs    Windows helper 替换与回滚

src/ui/tasks.rs  检查与下载后台任务；src/ui/mod.rs 持有 5 秒启动与 20 分钟定时器
```

- HTTP 使用 `ureq`：渠道版本检查是总预算 8 秒的小请求；更新系统的清单获取与文件下载不设总时长预算，连接建立限时 8 秒、单次读停滞 30 秒才中断。网络请求自动跟随代理：环境变量（`ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`）与 Windows 系统代理（注册表），关闭系统代理时直连。哈希使用 `sha2`；Ed25519 验签使用 `ring`（仅验证）。
- 下载写 `.part`，边下边算 SHA-256，完成后改名；失败删除 `.part`。
- 插件 zip 哈希与所用索引不符时自动刷新在线索引重试一次：缓存索引可能落后于服务器上重传过的产物。刷新后哈希与缓存一致（服务器产物与索引矛盾）或刷新失败（网络不可达）时按原始失配上报。
- 更新地址不写在 `config.json`：宿主用 `REPO_ROOT` 派生清单地址，本地测试用 `ASM_REPO_ROOT` 覆盖。
- 下载文件放在 `%LOCALAPPDATA%/agent-session-manager/updates/`，不写 agent 存储。
- 插件索引成功获取后缓存到 `plugins-index-cache.json`；`updates.json` 只保存 Agent 产品版本检查结果。

### App 更新流程

```text
1. 验签 app.json
2. 比较版本，低于或等于当前版本则忽略
3. 检查磁盘空间，流式下载 exe 到 updates/app/<version>/
4. 校验 SHA-256，不匹配删除并报错
5. 检查当前 exe 目录是否可写；不可写则退化为打开下载页
6. 生成 updates/app/update.cmd
7. app 启动 helper 后退出
8. helper 等待 PID 结束，备份旧 exe，复制新 exe，启动新版本
9. 失败时用 .bak 恢复并启动旧版本
```

- helper 参数全部加引号，支持中文与空格路径。
- 只等待本次 PID，不按映像名结束进程。
- 只保留一份 `.bak`，下次更新覆盖。
- 不修改用户数据目录中的配置、备份、导出。

### 插件更新流程

```text
1. 验签 plugins-index.json
2. 比较已安装插件版本与 min_host
3. 下载 zip 到 updates/plugins/
4. 校验 zip 的 SHA-256
5. 调用现有严格导入流程（manifest、exe SHA-256、图标、路径）
6. 激活新版本，保留原启用/停用状态
7. 刷新 registry、运行时 agent 列表、产品目录、安装扫描与版本源
8. 刷新插件页与会话页的 agent 信息
```

- 同一版本不重复下载；目标版本目录已存在时按现有导入规则处理。
- 导入成功后删除下载的 zip；失败保留错误信息，不留下半成品注册。
- 下载队列同一时间只处理一个插件，避免带宽与 UI 刷新互相干扰。

### 更新后的能力刷新

插件导入在 app 进程内完成，不需要额外 IPC。导入成功后：

- 重新读取 registry，激活新版本；
- 重建运行时 agent 列表与产品目录；
- 重新执行安装扫描与版本源组装；
- 刷新插件页、Agent 管理页、侧栏计数与会话页 agent 标签；
- 保留插件的启用/停用状态；
- 不重启 app。

### 并发与错误

- 自动检查与手动检查共用在途标记，正在检查时忽略新的同目标触发。
- 所有网络错误、验签失败、哈希失败都返回中文错误，保留 anyhow 上下文。
- 失败只更新状态与检查日志，不弹窗刷屏。
- 上次成功结果可继续展示，但必须标明本次检查失败。

## 本地测试

环境变量 `ASM_REPO_ROOT` 可在运行时覆盖 `REPO_ROOT`（末尾 `/` 自动去掉），不需要改代码重新编译。回环地址允许 HTTP，其他地址仍必须是 HTTPS：

```text
http://127.0.0.1:<port>
http://localhost:<port>
http://[::1]:<port>
```

本地测试服务器就是按线上路径摆文件的静态目录，用任意静态服务器即可：

```text
<root>/releases/latest/download/app.json
<root>/releases/latest/download/app.json.sig
<root>/releases/download/plugins/plugins-index.json
<root>/releases/download/plugins/plugins-index.json.sig
<root>/releases/download/plugins/<id>-<version>.zip
```

流程：

1. 生成测试用 Ed25519 密钥对；公钥经 `ASM_UPDATE_PUBKEY` 注入 app 构建。
2. 按「签名」一节的格式对 `app.json` 与 `plugins-index.json` 签名，与清单同目录摆放。
3. 启动静态服务器，例如 `python -m http.server 8765 --directory <root>`。
4. 设置 `ASM_REPO_ROOT=http://127.0.0.1:8765` 后启动 app，检查、下载、安装全部走本地服务器，插件索引无需改动。

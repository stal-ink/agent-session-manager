# 存储与数据

本文记录本工具自身的数据目录与备份约定。各 agent 的存储位置、读写格式、删除语义与已知限制由对应插件包说明，宿主不解析 agent 私有格式。

## 插件存储

- 插件包内说明自身存储与写入方式，入口见各包的 `README.md`。
- 宿主只通过协议调用插件的 `list`、`load`、`write`、`delete`、`destroy`、`active` 与 `backup_paths`。
- 备份范围由插件按运行时环境报告；宿主负责校验、复制到 app 数据目录与失败回滚。

## 本工具数据目录

位于 `%LOCALAPPDATA%/agent-session-manager/`。

### backups/

`backups/<时间戳>/` 保存原生写入前的目标存储完整副本。数据库备份包含 `-wal` 与 `-shm`；正文不在数据库里的存储同时备份正文目录。只保留最近 1 份，更早的在下次备份成功后自动清理。

### exports/

`exports/normal/` 与 `exports/handoff/` 分别保存普通文档与交接文档导出。

### ledger.json

转接台账。记录目标会话与上次转接时的源更新时间，用于判断是否需要覆盖。

### wallpapers/

`wallpapers/current.jpg` 是自定义壁纸主题使用的图片副本。导入时校正方向并裁为 16:10。替换时覆盖旧文件，不引用用户原始路径。

### config.json

用户配置：主题、壁纸文件名、两类导出开关、原生转接截断开关、自动检查更新、按产品的默认打开形态、插件根目录。损坏时保留现场并回退默认。主题可从旧版 `settings.json` 迁移。

### updates.json

版本检查结果缓存。按渠道记录最新版本、检查状态与失败信息。失败时保留上次成功版本，不含日志。

### plugins-index-cache.json

插件库索引缓存。保存最近一次成功获取的 `plugins-index.json`（含未安装插件条目、显示名与原始 `zip_url`）；缓存保留相对地址，启动时按当前索引目录展开，不绑定某个 hub。网络失败时未安装列表回退到该文件。它不是安装记录，损坏时忽略并等待下次成功检查。

### update-sources.json

可选覆盖文件。渠道默认值来自已安装插件 manifest 的 variant update 声明；该文件按渠道 id 覆盖 Agent 产品版本检查页 `version_url`、解析参数与 Agent 产品下载页 `download_url`，编辑后下次检查生效。`version_url` 与 `download_url` 都不是插件包地址；旧文件里的 `url` 仍按 `version_url` 读取。parser 只接受 `json`、`key`、`regex`、`html_regex`、`text`；旧版文件中的产品专用 parser 名会被忽略，解析规则回退到 manifest 默认值，地址覆盖仍然生效。缺失或损坏时忽略并使用 manifest 默认值，不生成模板。

### installs.json

安装扫描缓存：按形态记录已安装状态、版本与路径。旧缓存没有形态字段时视为单个桌面端形态，下次扫描补全。

### 产品目录

产品与形态目录不落盘：每次启动或插件变更后由已安装插件的 manifest 组装，包含检测器声明、显示名、产品色、默认打开建议与渠道 id。未安装插件时目录为空。

### plugin-registry.json

插件注册表，记录每个插件的 id、版本、协议、SHA-256 与相对于插件根目录的安装路径。只由 app.exe 原子写入。插件根目录可由 `config.json` 的 `plugins.root` 重映射。插件包内容与注册流程见 [plugin-architecture.md](plugin-architecture.md)。

### plugins/

导入后的插件目录，结构为 `plugins/<id>/<version>/`，每个目录包含 `plugin.toml`、`icon.png` 与插件 exe。导入暂存区为 `plugins/.staging/`。

### updates/

App 自更新与插件在线更新的下载与安装目录：`updates/app/<version>/`、`updates/plugins/` 与 `updates/app/update.cmd`。helper 备份的 `.bak` 只保留一份，安装成功后清理。

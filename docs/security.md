# 二进制加固与发布前检查

## 威胁模型与结论

本工具以单 exe 分发，读取并写入多个 agent 的本地会话。评估问题：只拿到 exe、没有源码与文档的逆向者，能否通过修改二进制加入新插件或新 agent。

1. 二进制补丁塞入新插件不可行：无导出表、无插件点；插件通过导入目录与 manifest 加载，exe 内没有可改写的适配器表；发布构建符号全部剥离。
2. 模仿交互自写同类工具可行：存储位置、SQL/JSONL schema、台账与备份语义可从字符串还原。SQL 与 UI 文案是功能必然，接受为残余风险。
3. 构建身份是主要泄漏：panic Location 携带绝对路径、工具链位置、镜像指纹与项目磁盘布局。

## 加固措施

- `[profile.release]`：`strip = true`、`lto = "thin"`、`codegen-units = 1`、`debug = false`。
- PDB 永不随包分发。`package.cmd` 只把 exe 放进 `dist/`。
- 路径重映射：`.cargo/config.local.toml`（不入库，模板见 `.cargo/config.local.example.toml`）把 panic Location 与调试路径中的构建机前缀映射为通用名。换构建机或移动仓库时必须同步更新映射。`RUSTFLAGS` 环境变量会整体覆盖 rustflags，不要混用。
- panic hook：release 构建下 panic 只输出消息本身，不输出文件路径与行号；debug 构建保留默认行为。
- 自更新与插件在线更新：`app.json`、`plugins-index.json` 必须用内置 Ed25519 公钥验签，下载文件再校验 SHA-256。
- 清单签名而非信任 GitHub：Release 资产的 digest 不是签名，发布账号被盗时 digest 会随恶意文件一起变更。清单把版本、哈希与下载地址绑定后用离线私钥签名，客户端内置公钥验证；同时避开匿名 GitHub API 每小时 60 次的限额。
- 测试、脚本与自动流程不调用真实销毁；真实存储验证需用户明确同意。

## 残余风险

| 残余 | 说明 | 不处理理由 |
|---|---|---|
| 相对模块路径 | panic Location 仍含 `src\...` 或 `plugins\...` 等相对路径，模块拓扑可推断 | 前缀重映射只能改写路径头部；彻底移除需 nightly `-Zlocation-detail=none` |
| SQL/JSONL 明文 | 各端存储读写语句可完整提取 | 传给 sqlite3_prepare 的语句必须存在；运行期混淆只能防脚本 |
| crate 版本指纹 | 依赖 crate 名与版本可推，等同 Cargo.lock 摘要 | remap 后保留尾段便于排障；CVE 匹配价值有限 |
| PE 时间戳 | 未做可复现构建归一化 | 单机构建场景无实际威胁 |

明确不采用：UPX 或 VMProtect 等壳、反调试 TLS callback、运行期字符串 XOR。这些手段误报率高、绕过成本低，收益与风险不匹配。

## 发布前检查

对 `dist/` 产物做字符串转储与 PE 检查：

1. `objdump --syms <exe>` 输出 `no symbols`。
2. 字符串全量提取后，仓库绝对路径、构建工具链目录、cargo 镜像域名、用户名与 `C:\Users` 的命中数必须为 0。具体前缀以 `.cargo/config.local.toml` 的映射为准。
3. PE Debug Directory 无 CodeView 或 RSDS 条目，无 `.pdb` 引用。
4. panic 位置只允许出现映射后的通用前缀与相对路径。

任何一项不通过，先修构建配置再发版。

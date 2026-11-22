//! 会话列表的纯逻辑：状态、筛选、排序与选择，不依赖 Slint 窗口。

use std::collections::{HashMap, HashSet};

use agent_session_manager::agents;
use agent_session_manager::catalog::Catalog;
use agent_session_manager::config::Config;
use agent_session_manager::installs::AgentInstall;
use agent_session_manager::ir::SessionSummary;
use agent_session_manager::open;
use agent_session_manager::paths;
use agent_session_manager::updates::{self, ChannelUpdate};

use super::assets::Icons;
use super::SelectionSummary;

pub struct State {
    pub all: Vec<SessionSummary>,
    pub filter: String,
    pub search: String,
    pub status_mask: u8,
    /// 排序方式：flat 平铺（默认），workspace 按工作区分组。
    pub sort_mode: String,
    /// 收起的工作区分组，键为 paths::key；默认全部展开。
    pub workspaces_collapsed: HashSet<String>,
    /// 刷新代次：只有当前代次的 Loaded 事件可以更新状态。
    pub refresh_gen: u32,
    /// 是否已有刷新任务在跑。
    pub refresh_running: bool,
    /// 刷新进行中又收到刷新请求时只记一次，当前任务完成后再补一轮。
    pub refresh_pending: bool,
    /// 最近一次加载中各 agent 的读取失败明细，界面用顶部提示条展示。
    pub session_errors: Vec<String>,
    /// 最近一次加载中各 agent 跳过的会话明细。
    pub session_warnings: Vec<String>,
    pub selected: HashSet<(String, String)>,
    pub installs: Vec<AgentInstall>,
    pub catalog: Catalog,
    pub plugins: Vec<agent_session_manager::plugins::InstalledPlugin>,
    pub plugin_importing: bool,
    /// 插件索引检查状态与结果；失败时保留上次成功结果但行内显示本次失败。
    pub plugin_checking: bool,
    pub plugin_last_check_ms: i64,
    pub plugin_check_error: String,
    pub plugin_updates: Vec<agent_session_manager::update::PluginEntry>,
    /// 插件库索引（含未安装条目），来自上次成功检查或本地缓存。
    pub plugin_index: Vec<agent_session_manager::update::PluginEntry>,
    /// 正在下载或导入的插件：id -> (已下载, 总量, 速度)。
    pub plugin_downloads: HashMap<String, (u64, u64, u64)>,
    /// 已下载完成、正在导入的插件 id。
    pub plugin_installing: HashSet<String>,
    /// 本次检查中导入失败的插件：id -> 错误。
    pub plugin_update_errors: HashMap<String, String>,
    /// 本次检查中导入成功的插件：id -> (新版本, 是否为更新)。
    pub plugin_updated: HashMap<String, (String, bool)>,
    /// 插件页是否已根据插件状态选过默认页签；只自动选一次。
    pub plugin_tab_initialized: std::cell::Cell<bool>,
    /// 插件检查与更新日志，只在内存保留最近 20 条。
    pub plugin_log: Vec<updates::LogEntry>,
    pub sources: Vec<updates::Source>,
    pub agents_scanning: bool,
    /// 收起的产品组；默认全部展开。
    pub groups_collapsed: HashSet<String>,
    pub garbage: Vec<(String, Vec<String>)>,
    pub garbage_scanned: bool,
    pub garbage_scanning: bool,
    pub config: Config,
    pub updates: Vec<ChannelUpdate>,
    pub update_log: Vec<updates::LogEntry>,
    pub checking: bool,
    pub last_check_ms: i64,
    /// App 自更新检查状态。
    pub app_checking: bool,
    pub app_update: Option<agent_session_manager::update::AppManifest>,
    pub app_update_error: String,
    pub app_last_check_ms: i64,
    /// 下载进度：已下载、总量、速度（字节/秒）。
    pub app_download: Option<(u64, u64, u64)>,
    pub app_downloaded: Option<std::path::PathBuf>,
    /// 已下载更新对应的版本；远端出现更高版本时作废旧下载。
    pub app_downloaded_version: Option<String>,
}

/// 顶部提示条摘要：失败与跳过分开，详细内容进弹窗。
pub fn session_issue_summary(errors: &[String], warnings: &[String]) -> String {
    let mut parts = Vec::new();
    if !errors.is_empty() {
        parts.push(format!("部分会话读取失败：{}", errors.join("；")));
    }
    if !warnings.is_empty() {
        parts.push(format!("{} 条会话未解析", warnings.len()));
    }
    parts.join("；")
}

/// 错误详情弹窗正文，保留每个 agent 的完整错误。
pub fn session_issue_details(errors: &[String], warnings: &[String]) -> String {
    let mut sections = Vec::new();
    if !errors.is_empty() {
        let lines: Vec<String> = errors.iter().map(|error| format!("· {error}")).collect();
        sections.push(format!("读取失败\n{}", lines.join("\n")));
    }
    if !warnings.is_empty() {
        let lines: Vec<String> = warnings
            .iter()
            .map(|warning| format!("· {warning}"))
            .collect();
        sections.push(format!("未解析\n{}", lines.join("\n")));
    }
    sections.join("\n\n")
}

/// 状态块：活跃 0、已归档 1、工作区归档/删除 2、已删除 3。
fn status_rank(s: &SessionSummary) -> u8 {
    if s.deleted {
        3
    } else if s.workspace_deleted {
        2
    } else if s.archived {
        1
    } else {
        0
    }
}

/// 产品可见性：至少一个形态已安装才可见；安装扫描尚未成功且无缓存时
/// 回退为全部可见，避免启动瞬间侧栏为空。
pub fn product_visible(state: &State, product: &str) -> bool {
    if state.installs.is_empty() {
        return true;
    }
    state
        .installs
        .iter()
        .find(|i| i.id == product)
        .map(|i| i.installed)
        .unwrap_or(false)
}

/// 当前筛选被隐藏时回退「全部会话」。
pub fn ensure_filter_visible(state: &mut State) {
    if state.filter != "all" && !product_visible(state, &state.filter) {
        state.filter = "all".into();
    }
}

/// 请求一轮刷新。运行中只记一个待办并返回 None；否则递增代次并返回新代次。
pub fn request_refresh(state: &mut State) -> Option<u32> {
    if state.refresh_running {
        state.refresh_pending = true;
        return None;
    }
    state.refresh_running = true;
    state.refresh_gen = state.refresh_gen.wrapping_add(1);
    Some(state.refresh_gen)
}

/// 当前代次结束。有待办时立刻开始下一代并返回其代次，否则结束刷新。
pub fn complete_refresh(state: &mut State) -> Option<u32> {
    state.refresh_running = false;
    if state.refresh_pending {
        state.refresh_pending = false;
        state.refresh_running = true;
        state.refresh_gen = state.refresh_gen.wrapping_add(1);
        Some(state.refresh_gen)
    } else {
        None
    }
}

/// 后台事件是否属于当前代次；过期代次直接丢弃，避免旧结果覆盖新结果。
pub fn refresh_is_current(state: &State, generation: u32) -> bool {
    state.refresh_gen == generation
}

pub fn session_matches(state: &State, session: &SessionSummary, q: &str) -> bool {
    product_visible(state, &session.agent)
        && (state.filter == "all" || session.agent == state.filter)
        && state.status_mask & (1 << status_rank(session)) != 0
        && (q.is_empty()
            || session.title.to_lowercase().contains(q)
            || session.cwd.to_lowercase().contains(q))
}

pub fn visible_keys(state: &State) -> Vec<(String, String)> {
    let q = state.search.to_lowercase();
    state
        .all
        .iter()
        .filter(|s| session_matches(state, s, &q))
        .map(|s| (s.agent.clone(), s.id.clone()))
        .collect()
}

/// 按 agent 汇总所选会话：保持首次出现顺序，同 agent 的 id 聚成一组。
pub fn group_by_agent(selected: Vec<(String, String)>) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (agent, id) in selected {
        if let Some((_, ids)) = groups.iter_mut().find(|(a, _)| a == &agent) {
            ids.push(id);
        } else {
            groups.push((agent, vec![id]));
        }
    }
    groups
}

pub fn selection_summary(state: &State, icons: &Icons) -> Vec<SelectionSummary> {
    let mut counts: HashMap<&str, i32> = HashMap::new();
    for (agent, _) in &state.selected {
        *counts.entry(agent.as_str()).or_default() += 1;
    }
    let mut summary: Vec<SelectionSummary> = agents::all()
        .iter()
        .filter_map(|agent| {
            counts.get(agent.id.as_ref()).map(|count| SelectionSummary {
                agent: agent.id.to_string().into(),
                label: agent.label.to_string().into(),
                count: *count,
                icon: icons.for_product(agent.id.as_ref(), &state.plugins),
            })
        })
        .collect();
    let known: Vec<String> = summary.iter().map(|item| item.agent.to_string()).collect();
    let mut unknown: Vec<(&str, i32)> = counts
        .into_iter()
        .filter(|(agent, _)| !known.iter().any(|known| known == agent))
        .collect();
    unknown.sort_by(|left, right| left.0.cmp(right.0));
    summary.extend(unknown.into_iter().map(|(agent, count)| SelectionSummary {
        agent: agent.into(),
        label: agents::label(agent).into(),
        count,
        icon: icons.for_product(agent, &state.plugins),
    }));
    summary
}

pub fn toggle_select_all(state: &mut State) {
    let visible = visible_keys(state);
    let all_selected =
        !visible.is_empty() && visible.iter().all(|key| state.selected.contains(key));
    if all_selected {
        state.selected.clear();
    } else {
        state.selected.extend(visible);
    }
}

/// 排序规则：活跃在前，其后依次为已归档、工作区归档/删除、已删除；
/// 每个状态块内按工作区（cwd）聚合，工作区分组键使用 `paths::key`，
/// 斜杠方向、大小写与末尾分隔符不参与比较；工作区按组内最新 updated_ms 倒序，
/// 组内按 updated_ms 倒序，同刻按 id 升序。
pub fn sort_sessions(all: &mut [SessionSummary]) {
    use std::cmp::Reverse;

    let mut newest: HashMap<(u8, String), i64> = HashMap::new();
    for s in all.iter() {
        let key = (status_rank(s), paths::key(&s.cwd));
        let e = newest.entry(key).or_insert(s.updated_ms);
        if s.updated_ms > *e {
            *e = s.updated_ms;
        }
    }
    all.sort_by_cached_key(|s| {
        let key = (status_rank(s), paths::key(&s.cwd));
        let group_newest = newest.get(&key).copied().unwrap_or(s.updated_ms);
        (
            key.0,
            Reverse(group_newest),
            key.1,
            Reverse(s.updated_ms),
            s.id.clone(),
        )
    });
}

/// 工作区排序：相同工作区聚在一起，组间按组内最新 updated_ms 倒序；
/// 组内仍按状态块（活跃→已归档→工作区→已删除）再按 updated_ms 倒序。
pub fn sort_by_workspace(all: &mut [SessionSummary]) {
    use std::cmp::Reverse;

    let mut newest: HashMap<String, i64> = HashMap::new();
    for s in all.iter() {
        let key = paths::key(&s.cwd);
        let e = newest.entry(key).or_insert(s.updated_ms);
        if s.updated_ms > *e {
            *e = s.updated_ms;
        }
    }
    all.sort_by_cached_key(|s| {
        let key = paths::key(&s.cwd);
        let group_newest = newest.get(&key).copied().unwrap_or(s.updated_ms);
        (
            Reverse(group_newest),
            key,
            status_rank(s),
            Reverse(s.updated_ms),
            s.id.clone(),
        )
    });
}

/// 插件声明 archive_from_open_ids 时，客户端打开的会话视为活跃，其余归入已归档。
/// 打开清单不可读（None）时不改写，保持读取端原状。
pub fn apply_open_ids_archive(all: &mut [SessionSummary], open: Option<&HashSet<String>>) {
    apply_open_ids_archive_with(all, open, |agent| {
        agents::get(agent)
            .and_then(|info| {
                info.manifest
                    .as_ref()
                    .map(|manifest| manifest.integration.clone())
            })
            .is_some_and(|integration| integration.archive_from_open_ids)
    });
}

fn apply_open_ids_archive_with(
    all: &mut [SessionSummary],
    open: Option<&HashSet<String>>,
    opted_in: impl Fn(&str) -> bool,
) {
    let Some(open) = open else { return };
    for session in all {
        if opted_in(&session.agent) && !open.contains(&session.id) {
            session.archived = true;
        }
    }
}

/// 搜索下拉的工作区建议：取当前 agent/状态/搜索词下可见会话的工作区，
/// 按组内最新 updated_ms 倒序，去重后最多 limit 个，返回 (名称, 路径) 与是否有更多。
#[cfg(test)]
pub fn workspace_suggestions(state: &State, limit: usize) -> (Vec<(String, String)>, bool) {
    let q = state.search.to_lowercase();
    workspace_suggestions_from(
        state.all.iter().filter(|s| session_matches(state, s, &q)),
        limit,
    )
}

/// 已在调用方筛选好的会话直接生成工作区建议，避免列表渲染时重复整表过滤。
pub fn workspace_suggestions_from<'a>(
    sessions: impl IntoIterator<Item = &'a SessionSummary>,
    limit: usize,
) -> (Vec<(String, String)>, bool) {
    use std::cmp::Reverse;
    let mut groups: HashMap<String, (i64, String)> = HashMap::new();
    for s in sessions {
        let key = paths::key(&s.cwd);
        let e = groups.entry(key).or_insert((s.updated_ms, s.cwd.clone()));
        if s.updated_ms > e.0 {
            *e = (s.updated_ms, s.cwd.clone());
        }
    }
    let mut ordered: Vec<(i64, String)> = groups.into_values().collect();
    ordered.sort_by_key(|(t, cwd)| (Reverse(*t), cwd.clone()));
    let more = ordered.len() > limit;
    let items = ordered
        .into_iter()
        .take(limit)
        .map(|(_, cwd)| (paths::name(&cwd), cwd))
        .collect();
    (items, more)
}

/// 已安装形态行的纯数据（图标在 view 层按产品补）。
pub struct VariantRow {
    pub variant: String,
    pub channel: String,
    pub label: String,
    pub version: String,
    pub latest: String,
    pub status: String,
    pub updatable: bool,
    pub latest_known: bool,
    pub action_enabled: bool,
    pub open_enabled: bool,
}

pub struct InstallGroup {
    pub product: String,
    pub label: String,
    pub installed_count: usize,
    pub show_header: bool,
    pub expanded: bool,
    pub rows: Vec<VariantRow>,
}

pub struct UninstallEntry {
    pub channel: String,
    pub product: String,
    pub label: String,
    pub latest: String,
    pub latest_known: bool,
    pub can_install: bool,
}

pub struct OpenDefault {
    pub product: String,
    pub label: String,
    pub summary: String,
    pub options: Vec<String>,
    pub current: String,
}

/// 渠道是否能检查：渠道非空且版本源里给出了非空检查 URL。
/// 只有下载页、没有检查 URL 的渠道（如 VS Code 扩展）不算可检查。
fn checkable(state: &State, channel: &str) -> bool {
    !channel.is_empty()
        && state
            .sources
            .iter()
            .find(|s| s.id == channel)
            .is_some_and(|s| !s.version_url.trim().is_empty())
}

/// 下载按钮对应的来源 id：可检查渠道取渠道源，不可检查时取形态自己的下载页。
/// 返回 None 表示没有可用下载页，操作列不显示按钮。
fn download_key(state: &State, variant_id: &str, channel: &str) -> Option<String> {
    let source = if channel.is_empty() {
        state.sources.iter().find(|s| s.id == variant_id)
    } else {
        state.sources.iter().find(|s| s.id == channel)
    }?;
    (!source.download_url.trim().is_empty()).then(|| source.id.clone())
}

/// 渠道检查结果派生：最新版本、是否已知、失败、错误文本。
/// 失败时保留上一次成功的最新版本作为「上次已知」。
fn channel_updates(update: Option<&ChannelUpdate>) -> (String, bool, bool, String) {
    let (latest, ok, error) = match update {
        Some(u) => (u.latest.clone(), u.ok, u.error.clone()),
        None => (String::new(), false, String::new()),
    };
    let latest_known = !latest.is_empty();
    let failed = update.is_some() && !ok;
    (latest, latest_known, failed, error)
}

/// 下载速度按实际量级选单位：B/s、KB/s、MB/s，小值不进位到 MB。
pub fn format_speed(speed_bps: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let speed = speed_bps as f64;
    if speed < KIB {
        format!("{} B/s", speed_bps)
    } else if speed < MIB {
        format!("{:.1} KB/s", speed / KIB)
    } else {
        format!("{:.1} MB/s", speed / MIB)
    }
}

/// 侧栏版本号旁的更新按钮状态：0 隐藏 / 2 可更新 / 4 下载中 / 5 已下载。
/// 只有明确发现新版本（或正在下载、已下载）时才显示；检查中、失败、无更新都不显示。
pub fn app_update_sidebar(state: &State) -> (i32, String) {
    if let Some((downloaded, total, speed)) = state.app_download {
        let progress = if total == 0 {
            0.0
        } else {
            downloaded as f32 / total as f32
        };
        return (
            4,
            format!(
                "正在下载更新 {:.0}% · {}",
                progress * 100.0,
                format_speed(speed)
            ),
        );
    }
    if state.app_downloaded.is_some() {
        return (5, "更新已下载，点击安装".into());
    }
    // 检查失败或没有新版本：不显示更新按钮。
    if !state.app_update_error.is_empty() {
        return (0, String::new());
    }
    if let Some(update) = &state.app_update {
        return (2, format!("发现新版本 v{}，可更新", update.version));
    }
    (0, String::new())
}

/// 插件行更新状态：文案、样式类别、更新按钮是否可点。
/// 类别：0 未检查 / 1 检查中 / 2 可更新 / 3 下载中 / 4 最新 / 5 失败 / 6 已更新。
pub fn plugin_status(
    state: &State,
    plugin: &agent_session_manager::plugins::InstalledPlugin,
) -> (String, i32, bool) {
    if let Some((downloaded, total, _speed)) = state.plugin_downloads.get(&plugin.id) {
        let status = if *total == 0 {
            "更新中…".to_string()
        } else {
            format!(
                "更新中 {:.0}%",
                (*downloaded as f32 / *total as f32 * 100.0).clamp(0.0, 100.0)
            )
        };
        return (status, 3, false);
    }
    if state.plugin_installing.contains(&plugin.id) {
        return ("安装中…".into(), 3, false);
    }
    if state.plugin_checking {
        return ("检查中…".into(), 1, false);
    }
    if state.plugin_update_errors.contains_key(&plugin.id) {
        return ("更新失败".into(), 5, false);
    }
    if let Some((_, updated)) = state.plugin_updated.get(&plugin.id) {
        return (if *updated { "已更新" } else { "已安装" }.into(), 6, false);
    }
    if state
        .plugin_updates
        .iter()
        .any(|entry| entry.id == plugin.id)
    {
        return ("可更新".into(), 2, true);
    }
    if !state.plugin_check_error.is_empty() {
        return ("检查失败".into(), 5, false);
    }
    if state.plugin_last_check_ms > 0 {
        return ("最新".into(), 4, false);
    }
    ("未检查".into(), 0, false)
}

fn status_rank_of(row: &VariantRow) -> u8 {
    if row.updatable {
        0
    } else if row.status == "最新" {
        1
    } else {
        2
    }
}

fn catalog_product<'a>(
    state: &'a State,
    id: &str,
) -> Option<&'a agent_session_manager::catalog::CatalogProduct> {
    state.catalog.products.iter().find(|p| p.id == id)
}

/// 已安装区：按产品分组；≥2 个已安装形态才显示小标题。
/// 排序：带分组标题的产品组置顶；组与单形态行各自按任一形态可更新、
/// 最新、未知/检查失败排序，组内同规则。
pub fn installed_groups(state: &State) -> Vec<InstallGroup> {
    let mut groups: Vec<(u8, usize, InstallGroup)> = Vec::new();
    for (index, install) in state.installs.iter().enumerate() {
        let installed: Vec<_> = install.variants.iter().filter(|v| v.installed).collect();
        if installed.is_empty() {
            continue;
        }
        let mut rows: Vec<(u8, usize, VariantRow)> = Vec::new();
        for (vindex, v) in installed.iter().enumerate() {
            let update = state.updates.iter().find(|u| u.id == v.channel);
            let download = download_key(state, &v.id, &v.channel);
            let has_download = download.is_some();
            let (latest, latest_known) = if checkable(state, &v.channel) {
                let (latest, latest_known, _failed, _error) = channel_updates(update);
                (latest, latest_known)
            } else {
                (String::new(), false)
            };
            // 已安装行统一显示下载图标：可更新且有下载页时亮，否则灰；检查失败与不可检查都不显示按钮以外的标记。
            let status = if !checkable(state, &v.channel) {
                "—"
            } else {
                match update.filter(|u| u.ok && !u.latest.is_empty()) {
                    Some(_) if v.version.trim().is_empty() => "未知",
                    Some(u)
                        if updates::compare(&u.latest, &v.version)
                            == std::cmp::Ordering::Greater =>
                    {
                        "可更新"
                    }
                    Some(_) => "最新",
                    None => "未知",
                }
            };
            let row = VariantRow {
                variant: v.id.clone(),
                channel: download.unwrap_or_default(),
                label: v.kind.label().to_string(),
                version: v.version.clone(),
                latest,
                status: status.to_string(),
                updatable: status == "可更新",
                latest_known,
                // 有新版但下载页缺失时同样灰禁用，避免亮按钮点击无动作。
                action_enabled: status == "可更新" && has_download,
                open_enabled: open::variant_openable(&install.id, &v.kind),
            };
            rows.push((status_rank_of(&row), vindex, row));
        }
        rows.sort_by_key(|(rank, vindex, _)| (*rank, *vindex));
        let rows: Vec<VariantRow> = rows.into_iter().map(|(_, _, row)| row).collect();
        let rank = rows.first().map(status_rank_of).unwrap_or(2);
        let product = catalog_product(state, &install.id);
        groups.push((
            rank,
            index,
            InstallGroup {
                product: install.id.to_string(),
                label: product
                    .map(|p| p.label.clone())
                    .unwrap_or_else(|| install.label.to_string()),
                installed_count: rows.len(),
                show_header: rows.len() >= 2,
                expanded: !state.groups_collapsed.contains(&install.id),
                rows,
            },
        ));
    }
    // 带分组标题的产品组置顶：组之间仍按可更新/最新/未知排序，单形态行同理。
    groups.sort_by_key(|(rank, index, group)| (u8::from(!group.show_header), *rank, *index));
    groups.into_iter().map(|(_, _, group)| group).collect()
}

/// 未安装区：按 catalog 注册顺序平铺，不参与已安装区排序。
pub fn uninstalled_entries(state: &State) -> Vec<UninstallEntry> {
    let mut entries = Vec::new();
    for install in &state.installs {
        for v in install.variants.iter().filter(|v| !v.installed) {
            let update = state.updates.iter().find(|u| u.id == v.channel);
            let download = download_key(state, &v.id, &v.channel);
            let (latest, latest_known) = if checkable(state, &v.channel) {
                let (latest, latest_known, _failed, _error) = channel_updates(update);
                (latest, latest_known)
            } else {
                (String::new(), false)
            };
            entries.push(UninstallEntry {
                channel: download.clone().unwrap_or_default(),
                product: install.id.to_string(),
                label: v.kind.label().to_string(),
                latest,
                latest_known,
                // 有下载页就能装，不依赖检查结果；没有下载页时按钮灰禁用而非隐藏。
                can_install: download.is_some(),
            });
        }
    }
    entries
}

/// 设置「打开」行：只列已安装且可打开形态 ≥ 2 的产品。
pub fn open_defaults(state: &State) -> Vec<OpenDefault> {
    let mut rows = Vec::new();
    for install in &state.installs {
        let openable: Vec<_> = install
            .variants
            .iter()
            .filter(|v| v.installed && open::variant_openable(&install.id, &v.kind))
            .collect();
        if openable.len() < 2 {
            continue;
        }
        let installed: Vec<_> = install.variants.iter().filter(|v| v.installed).collect();
        let summary = format!(
            "已安装 {} 项：{}",
            installed.len(),
            installed
                .iter()
                .map(|v| v.kind.short_label())
                .collect::<Vec<_>>()
                .join(" / ")
        );
        let current = open::resolve_open_variant(
            &install.id,
            &state.installs,
            state
                .config
                .open
                .defaults
                .get(&install.id)
                .map(String::as_str),
        )
        .and_then(|id| install.variants.iter().find(|v| v.id == id))
        .map(|v| v.kind.label().to_string())
        .unwrap_or_default();
        rows.push(OpenDefault {
            product: install.id.to_string(),
            label: install.label.to_string(),
            summary,
            options: openable
                .iter()
                .map(|v| v.kind.label().to_string())
                .collect(),
            current,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, cwd: &str, updated_ms: i64, archived: bool) -> SessionSummary {
        SessionSummary {
            agent: "qoder".into(),
            id: id.to_string(),
            title: id.to_string(),
            cwd: cwd.to_string(),
            created_ms: 0,
            updated_ms,
            message_count: 0,
            archived,
            deleted: false,
            workspace_deleted: false,
        }
    }

    fn ids(all: &[SessionSummary]) -> Vec<&str> {
        all.iter().map(|x| x.id.as_str()).collect()
    }

    #[test]
    fn active_and_archived_never_interleave() {
        let mut all = vec![
            s("a1", "/w1", 500, false),
            s("b1", "/w2", 900, true),
            s("a2", "/w1", 700, true),
            s("b2", "/w2", 100, false),
            s("a3", "/w3", 300, false),
            s("b3", "/w3", 50, true),
        ];
        sort_sessions(&mut all);
        let flags: Vec<bool> = all.iter().map(|x| x.archived).collect();
        assert_eq!(flags, vec![false, false, false, true, true, true]);
    }

    #[test]
    fn same_workspace_stays_contiguous() {
        let mut all = vec![
            s("a1", r"C:\proj", 100, false),
            s("b1", r"D:\other", 300, false),
            s("a2", r"C:\proj", 200, false),
            s("b2", r"D:\other", 150, false),
        ];
        sort_sessions(&mut all);
        // D:\other 组最新 300 > C:\proj 组最新 200，故 D 组整体在前，组内各自倒序。
        assert_eq!(ids(&all), vec!["b1", "b2", "a2", "a1"]);
    }

    #[test]
    fn groups_ordered_by_newest_update_then_inner_desc() {
        let mut all = vec![
            s("a_old", "/a", 100, false),
            s("b_new", "/b", 900, false),
            s("a_new", "/a", 500, false),
            s("c_mid", "/c", 300, false),
        ];
        sort_sessions(&mut all);
        assert_eq!(ids(&all), vec!["b_new", "a_new", "a_old", "c_mid"]);
    }

    #[test]
    fn cwd_separators_case_and_trailing_separator_normalized() {
        let mut all = vec![
            s("x1", "C:/Work/Proj", 100, false),
            s("y1", r"D:\Else", 200, false),
            s("x2", "c:\\work\\proj\\", 300, false),
        ];
        sort_sessions(&mut all);
        assert_eq!(ids(&all), vec!["x2", "x1", "y1"]);
    }

    #[test]
    fn empty_cwd_is_its_own_group() {
        let mut all = vec![
            s("e1", "", 100, false),
            s("r1", "/", 200, false),
            s("e2", "", 300, false),
            s("r2", "\\", 50, false),
        ];
        sort_sessions(&mut all);
        assert_eq!(ids(&all), vec!["e2", "e1", "r1", "r2"]);
    }

    #[test]
    fn equal_timestamps_fall_back_to_id_ascending() {
        let mut all = vec![
            s("b", "/w", 100, false),
            s("a", "/w", 100, false),
            s("c", "/w", 100, false),
        ];
        sort_sessions(&mut all);
        assert_eq!(ids(&all), vec!["a", "b", "c"]);
    }

    #[test]
    fn blocks_order_active_archived_workspace_deleted_deleted() {
        let mut gone = s("gone", "/w", 100, false);
        gone.workspace_deleted = true;
        let mut tomb = s("tomb", "/w", 200, true);
        tomb.deleted = true;
        tomb.workspace_deleted = true;
        let mut all = vec![
            tomb,
            s("active", "/w", 300, false),
            gone,
            s("archived", "/w", 400, true),
        ];
        sort_sessions(&mut all);
        assert_eq!(ids(&all), vec!["active", "archived", "gone", "tomb"]);
    }

    fn source(id: &str, url: &str) -> updates::Source {
        updates::Source {
            id: id.into(),
            product: String::new(),
            label: String::new(),
            version_url: url.into(),
            parser: String::new(),
            download_url: "https://example.com/download".into(),
            ..Default::default()
        }
    }

    fn state_with(all: Vec<SessionSummary>) -> State {
        State {
            all,
            filter: "all".into(),
            search: String::new(),
            status_mask: 0b1111,
            sort_mode: "flat".into(),
            workspaces_collapsed: HashSet::new(),
            refresh_gen: 0,
            refresh_running: false,
            refresh_pending: false,
            session_errors: Vec::new(),
            session_warnings: Vec::new(),
            selected: HashSet::new(),
            installs: Vec::new(),
            catalog: Catalog::default(),
            plugins: Vec::new(),
            plugin_importing: false,
            plugin_checking: false,
            plugin_last_check_ms: 0,
            plugin_check_error: String::new(),
            plugin_updates: Vec::new(),
            plugin_index: Vec::new(),
            plugin_downloads: HashMap::new(),
            plugin_installing: HashSet::new(),
            plugin_update_errors: HashMap::new(),
            plugin_updated: HashMap::new(),
            plugin_tab_initialized: std::cell::Cell::new(false),
            plugin_log: Vec::new(),
            sources: vec![
                source("qoder-desktop", "https://example.com/qoder"),
                source("zcode-desktop", "https://example.com/zcode"),
                source("cursor-desktop", "https://example.com/cursor"),
                source("workbuddy-desktop", "https://example.com/workbuddy"),
                source("kiro-desktop", "https://example.com/kiro"),
                source("claude-cli", "https://example.com/claude"),
                source("dsh-desktop", "https://example.com/dsh"),
                // VS Code 扩展走统一的 Marketplace 渠道。
                source(
                    "claude-vscode",
                    "https://marketplace.visualstudio.com/items?itemName=anthropic.claude-code",
                ),
            ],
            agents_scanning: false,
            groups_collapsed: HashSet::new(),
            garbage: Vec::new(),
            garbage_scanned: false,
            garbage_scanning: false,
            config: agent_session_manager::config::Config::default(),
            updates: Vec::new(),
            update_log: Vec::new(),
            checking: false,
            last_check_ms: 0,
            app_checking: false,
            app_update: None,
            app_update_error: String::new(),
            app_last_check_ms: 0,
            app_download: None,
            app_downloaded: None,
            app_downloaded_version: None,
        }
    }

    fn multi(agent: &str, id: &str, archived: bool, deleted: bool) -> SessionSummary {
        let mut s = s(id, "/w", 100, archived);
        s.agent = agent.into();
        s.deleted = deleted;
        s
    }

    #[test]
    fn kiro_history_becomes_archived_and_open_stays_active() {
        let mut all = vec![
            multi("kiro", "open", false, false),
            multi("kiro", "history", false, false),
            multi("qoder", "q", false, false),
        ];
        let mut open = HashSet::new();
        open.insert("open".to_string());
        apply_open_ids_archive_with(&mut all, Some(&open), |agent| agent == "kiro");
        assert!(!all[0].archived);
        assert!(all[1].archived);
        assert!(!all[2].archived);

        // 面板不可读时不改写。
        let mut untouched = vec![multi("kiro", "history", false, false)];
        apply_open_ids_archive_with(&mut untouched, None, |agent| agent == "kiro");
        assert!(!untouched[0].archived);
    }

    #[test]
    fn workspace_sort_groups_then_status_inside() {
        let mut all = vec![
            s("a_active", "/a", 100, false),
            s("b_active", "/b", 900, false),
            s("a_archived", "/a", 800, true),
            s("b_archived", "/b", 50, true),
        ];
        sort_by_workspace(&mut all);
        // /b 组最新 900 在前，/a 组最新 800 在后；组内活跃先于归档。
        assert_eq!(
            ids(&all),
            vec!["b_active", "b_archived", "a_active", "a_archived"]
        );
    }

    #[test]
    fn refresh_requests_merge_and_stale_results_are_ignored() {
        let mut state = state_with(Vec::new());
        let first = request_refresh(&mut state).expect("首次刷新应立即开始");
        assert!(refresh_is_current(&state, first));
        assert_eq!(request_refresh(&mut state), None);
        assert!(state.refresh_pending);
        assert_eq!(request_refresh(&mut state), None);

        let second = complete_refresh(&mut state).expect("有待办时结束当前代次应补一轮");
        assert_ne!(second, first);
        assert!(!refresh_is_current(&state, first));
        assert!(refresh_is_current(&state, second));
        assert_eq!(complete_refresh(&mut state), None);
        assert!(!state.refresh_running);
    }

    #[test]
    fn workspace_suggestions_respect_filter_and_cap() {
        let mut state = state_with(vec![
            s("a1", "/alpha", 900, false),
            s("a2", "/alpha", 100, false),
            s("b1", "/beta", 800, false),
            s("c1", "/gamma", 700, false),
            s("d1", "/delta", 600, false),
            s("e1", "/epsilon", 500, false),
            s("f1", "/zeta", 400, false),
        ]);
        let (items, more) = workspace_suggestions(&state, 5);
        assert!(more);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].0, "alpha");
        assert_eq!(items[0].1, "/alpha");

        // 切换 agent 筛选后建议随之变化。
        state.all.push(multi("zcode", "z1", false, false));
        state.filter = "zcode".into();
        let (items, more) = workspace_suggestions(&state, 5);
        assert!(!more);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].1, "/w");

        // 搜索词参与建议过滤：只保留命中的会话所属工作区。
        state.filter = "all".into();
        state.search = "alpha".into();
        let (items, more) = workspace_suggestions(&state, 5);
        assert!(!more);
        assert_eq!(items, vec![("alpha".to_string(), "/alpha".to_string())]);
    }

    #[test]
    fn select_all_respects_filters_and_toggles() {
        let mut state = state_with(vec![
            multi("qoder", "a", false, false),
            multi("zcode", "b", true, false),
            multi("workbuddy", "c", false, true),
        ]);
        toggle_select_all(&mut state);
        assert_eq!(state.selected.len(), 3);
        toggle_select_all(&mut state);
        assert!(state.selected.is_empty());

        state.status_mask = 1;
        toggle_select_all(&mut state);
        assert_eq!(state.selected.len(), 1);
        assert!(state
            .selected
            .contains(&("qoder".to_string(), "a".to_string())));

        state.selected.clear();
        state.search = "missing".into();
        toggle_select_all(&mut state);
        assert!(state.selected.is_empty());
    }

    #[test]
    fn selection_summary_groups_selected_by_agent() {
        let mut state = state_with(vec![
            multi("qoder", "a", false, false),
            multi("qoder", "b", false, false),
            multi("zcode", "c", false, false),
        ]);
        state.selected.insert(("qoder".into(), "a".into()));
        state.selected.insert(("qoder".into(), "b".into()));
        state.selected.insert(("zcode".into(), "c".into()));
        let summary = selection_summary(
            &state,
            &Icons {
                by_agent: HashMap::new(),
            },
        );
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].agent.as_str(), "qoder");
        assert_eq!(summary[0].count, 2);
        assert_eq!(summary[1].agent.as_str(), "zcode");
        assert_eq!(summary[1].count, 1);
    }

    #[test]
    fn workspace_order_is_computed_per_status_block() {
        let mut all = vec![
            s("a_active", "/a", 200, false),
            s("b_active", "/b", 300, false),
            s("a_archived", "/a", 900, true),
            s("b_archived", "/b", 100, true),
        ];
        sort_sessions(&mut all);
        // 活跃块：/b 最新 300 在 /a 最新 200 之前；归档块：/a 最新 900 在 /b 最新 100 之前。
        assert_eq!(
            ids(&all),
            vec!["b_active", "a_active", "a_archived", "b_archived"]
        );
    }

    #[test]
    fn group_by_agent_keeps_first_seen_order() {
        let groups = group_by_agent(vec![
            ("zcode".into(), "b".into()),
            ("qoder".into(), "a".into()),
            ("zcode".into(), "c".into()),
        ]);
        assert_eq!(
            groups,
            vec![
                ("zcode".to_string(), vec!["b".to_string(), "c".to_string()]),
                ("qoder".to_string(), vec!["a".to_string()]),
            ]
        );
    }

    use agent_session_manager::installs::{InstallKind, InstallVariant};

    fn variant(
        id: &str,
        product: &str,
        kind: InstallKind,
        installed: bool,
        version: &str,
        channel: &str,
    ) -> InstallVariant {
        InstallVariant {
            id: id.into(),
            product: product.into(),
            kind,
            version: version.into(),
            path: String::new(),
            installed,
            channel: channel.into(),
        }
    }

    fn install(
        id: &'static str,
        label: &'static str,
        variants: Vec<InstallVariant>,
        default_open: Option<&str>,
    ) -> AgentInstall {
        let installed = variants.iter().any(|v| v.installed);
        let version = variants
            .iter()
            .find(|v| v.installed)
            .map(|v| v.version.clone())
            .unwrap_or_default();
        AgentInstall {
            id: id.to_string(),
            label: label.to_string(),
            installed,
            version,
            default_open_variant: default_open.map(str::to_string),
            variants,
        }
    }

    fn channel(id: &str, latest: &str, ok: bool) -> ChannelUpdate {
        ChannelUpdate {
            id: id.into(),
            latest: latest.into(),
            checked_at: 1,
            ok,
            error: if ok {
                String::new()
            } else {
                "网络请求失败".into()
            },
            download_url: "https://example.com".into(),
        }
    }

    fn claude_two(installed_vs: bool, installed_cli: bool) -> AgentInstall {
        install(
            "claude",
            "Claude Code",
            vec![
                variant(
                    "claude-vscode",
                    "claude",
                    InstallKind::Vsix("Claude Code for VS Code".into()),
                    installed_vs,
                    "2.1.325",
                    "claude-vscode",
                ),
                variant(
                    "claude-cli",
                    "claude",
                    InstallKind::Cli("Claude Code CLI".into()),
                    installed_cli,
                    "2.1.326",
                    "claude-cli",
                ),
            ],
            Some("claude-vscode"),
        )
    }

    #[test]
    fn visibility_falls_back_to_all_without_cache() {
        let state = state_with(vec![]);
        assert!(product_visible(&state, "qoder"));
        assert!(product_visible(&state, "claude"));
    }

    #[test]
    fn uninstalled_product_hidden_from_sessions_and_filter() {
        let mut state = state_with(vec![
            multi("kiro", "k", false, false),
            multi("qoder", "q", false, false),
        ]);
        state.installs = vec![
            install(
                "kiro",
                "Kiro",
                vec![variant(
                    "kiro-desktop",
                    "kiro",
                    InstallKind::Desktop("Kiro".into()),
                    true,
                    "1.4.121",
                    "kiro-desktop",
                )],
                None,
            ),
            install(
                "qoder",
                "Qoder",
                vec![variant(
                    "qoder-desktop",
                    "qoder",
                    InstallKind::Desktop("Qoder".into()),
                    false,
                    "",
                    "qoder-desktop",
                )],
                None,
            ),
        ];
        assert!(product_visible(&state, "kiro"));
        assert!(!product_visible(&state, "qoder"));
        assert_eq!(visible_keys(&state).len(), 1);
        state.filter = "qoder".into();
        ensure_filter_visible(&mut state);
        assert_eq!(state.filter, "all");
    }

    #[test]
    fn two_installed_forms_show_header_single_does_not() {
        let mut state = state_with(vec![]);
        state.installs = vec![
            claude_two(true, true),
            install(
                "qoder",
                "Qoder",
                vec![variant(
                    "qoder-desktop",
                    "qoder",
                    InstallKind::Desktop("Qoder".into()),
                    true,
                    "0.5.7",
                    "qoder-desktop",
                )],
                None,
            ),
        ];
        let groups = installed_groups(&state);
        let claude = groups.iter().find(|g| g.product == "claude").unwrap();
        assert!(claude.show_header);
        assert_eq!(claude.installed_count, 2);
        assert!(claude.expanded);
        assert_eq!(claude.rows[0].label, "Claude Code for VS Code");
        let qoder = groups.iter().find(|g| g.product == "qoder").unwrap();
        assert!(!qoder.show_header);
        // 单形态产品同样提供启动与下载图标；未检查到结果时下载灰色禁用。
        assert!(qoder.rows[0].open_enabled);
        assert!(!qoder.rows[0].action_enabled);
    }

    #[test]
    fn uninstalled_form_only_in_uninstalled_section() {
        let mut state = state_with(vec![]);
        state.installs = vec![claude_two(false, true)];
        let groups = installed_groups(&state);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].rows.len(), 1);
        assert!(!groups[0].show_header);
        let entries = uninstalled_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Claude Code for VS Code");
        // 没有检查接口但有下载页：保留安装入口，最新版本不伪造。
        assert!(entries[0].can_install);
        assert!(!entries[0].latest_known);
        assert_eq!(entries[0].channel, "claude-vscode");
    }

    #[test]
    fn color_groups_pinned_then_updatable_first() {
        let mut state = state_with(vec![]);
        state.installs = vec![
            claude_two(true, true),
            install(
                "qoder",
                "Qoder",
                vec![variant(
                    "qoder-desktop",
                    "qoder",
                    InstallKind::Desktop("Qoder".into()),
                    true,
                    "0.5.7",
                    "qoder-desktop",
                )],
                None,
            ),
        ];
        state.updates = vec![
            channel("claude-cli", "2.1.326", true),
            channel("qoder-desktop", "0.5.11", true),
        ];
        let groups = installed_groups(&state);
        // 带分组标题的产品组置顶，即使单形态行可更新也排在其后。
        assert_eq!(groups[0].product, "claude");
        assert!(groups[0].show_header);
        assert_eq!(groups[0].rows[0].label, "Claude Code CLI");
        assert_eq!(groups[1].product, "qoder");
        assert!(groups[1].rows[0].updatable);
        assert!(groups[1].rows[0].action_enabled);
        let claude = groups.iter().find(|g| g.product == "claude").unwrap();
        let cli = claude
            .rows
            .iter()
            .find(|r| r.label == "Claude Code CLI")
            .unwrap();
        assert_eq!(cli.status, "最新");
        assert!(!cli.action_enabled);
        let vs = claude
            .rows
            .iter()
            .find(|r| r.label == "Claude Code for VS Code")
            .unwrap();
        // 扩展渠道同样可检查：没有检查结果时是未知 + 灰色下载。
        assert_eq!(vs.status, "未知");
        assert!(!vs.action_enabled);
    }

    #[test]
    fn update_button_disabled_when_download_page_missing() {
        let mut state = state_with(vec![]);
        state.installs = vec![install(
            "qoder",
            "Qoder",
            vec![variant(
                "qoder-desktop",
                "qoder",
                InstallKind::Desktop("Qoder".into()),
                true,
                "0.5.7",
                "qoder-desktop",
            )],
            None,
        )];
        state.updates = vec![channel("qoder-desktop", "0.5.11", true)];
        for source in &mut state.sources {
            if source.id == "qoder-desktop" {
                source.download_url.clear();
            }
        }
        let groups = installed_groups(&state);
        // 有新版但下载页缺失：保留灰色下载图标，点击不会发生。
        assert_eq!(groups[0].rows[0].status, "可更新");
        assert!(!groups[0].rows[0].action_enabled);
        assert_eq!(groups[0].rows[0].channel, "");
    }

    #[test]
    fn failed_check_keeps_last_known_latest_without_failure_mark() {
        let mut state = state_with(vec![]);
        state.installs = vec![claude_two(true, true)];
        state.updates = vec![channel("claude-cli", "2.1.326", false)];
        let groups = installed_groups(&state);
        let cli = groups[0]
            .rows
            .iter()
            .find(|r| r.label == "Claude Code CLI")
            .unwrap();
        // 失败信息只进浮窗日志；行内退回「未知」，下载图标灰。
        assert_eq!(cli.status, "未知");
        assert!(cli.latest_known);
        assert_eq!(cli.latest, "2.1.326");
        assert!(!cli.action_enabled);
    }

    #[test]
    fn uninstalled_install_button_follows_channel_state() {
        let mut state = state_with(vec![]);
        let kiro = install(
            "kiro",
            "Kiro",
            vec![variant(
                "kiro-desktop",
                "kiro",
                InstallKind::Desktop("Kiro".into()),
                false,
                "",
                "kiro-desktop",
            )],
            None,
        );
        state.installs = vec![kiro.clone()];
        state.updates = vec![channel("kiro-desktop", "1.4.154", true)];
        let entries = uninstalled_entries(&state);
        assert!(entries[0].can_install);
        assert!(entries[0].latest_known);
        state.updates = vec![channel("kiro-desktop", "1.4.154", false)];
        let entries = uninstalled_entries(&state);
        // 安装不依赖检查结果：有下载页就能装，失败信息只进浮窗日志。
        assert!(entries[0].can_install);
        // 连下载源都没有时按钮灰禁用（仍保留「安装」占位）。
        state.sources.clear();
        let entries = uninstalled_entries(&state);
        assert!(!entries[0].can_install);
        assert!(!entries[0].latest_known);
        assert_eq!(entries[0].channel, "");
    }

    #[test]
    fn collapsed_group_stays_collapsed() {
        let mut state = state_with(vec![]);
        state.installs = vec![claude_two(true, true)];
        state.groups_collapsed.insert("claude".into());
        let groups = installed_groups(&state);
        assert!(!groups[0].expanded);
    }

    #[test]
    fn open_defaults_only_for_multi_openable_products() {
        let mut state = state_with(vec![]);
        state.installs = vec![
            claude_two(true, true),
            install(
                "cursor",
                "Cursor",
                vec![variant(
                    "cursor-desktop",
                    "cursor",
                    InstallKind::Desktop("Cursor".into()),
                    true,
                    "3.29.15",
                    "cursor-desktop",
                )],
                None,
            ),
        ];
        let rows = open_defaults(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].product, "claude");
        assert_eq!(rows[0].options.len(), 2);
        assert_eq!(rows[0].current, "Claude Code for VS Code");
        assert_eq!(rows[0].summary, "已安装 2 项：VS Code 扩展 / CLI");
        // 用户设置优先于 catalog 建议。
        state
            .config
            .open
            .defaults
            .insert("claude".into(), "claude-cli".into());
        let rows = open_defaults(&state);
        assert_eq!(rows[0].current, "Claude Code CLI");
        // 只剩一个已安装形态时整行消失。
        state.installs = vec![claude_two(false, true)];
        assert!(open_defaults(&state).is_empty());
    }

    #[test]
    fn session_issue_summary_and_details_separate_failures_from_skips() {
        let errors = vec!["Trae CN（密钥获取失败）".to_string()];
        let warnings = vec![
            "Trae CN（会话 a 无法解析）".to_string(),
            "Trae CN（会话 b 无法解析）".to_string(),
        ];
        let summary = session_issue_summary(&errors, &warnings);
        assert!(summary.contains("部分会话读取失败"));
        assert!(summary.contains("2 条会话未解析"));
        let details = session_issue_details(&errors, &warnings);
        assert!(details.contains("读取失败\n· Trae CN（密钥获取失败）"));
        assert!(details.contains("未解析\n· Trae CN（会话 a 无法解析）"));
        assert_eq!(session_issue_summary(&[], &[]), "");
        assert_eq!(session_issue_details(&[], &[]), "");
    }

    fn installed_plugin(
        id: &str,
        version: &str,
    ) -> agent_session_manager::plugins::InstalledPlugin {
        agent_session_manager::plugins::InstalledPlugin {
            id: id.into(),
            label: id.into(),
            version: version.into(),
            root: format!("{id}/{version}"),
            enabled: true,
            can_write: true,
            variants: 1,
            icon: Vec::new(),
        }
    }

    fn plugin_entry(id: &str, version: &str) -> agent_session_manager::update::PluginEntry {
        agent_session_manager::update::PluginEntry {
            id: id.into(),
            label: id.into(),
            version: version.into(),
            zip_url: "https://example.com/plugin.zip".into(),
            sha256: "a".repeat(64),
            size: 200,
            min_host: String::new(),
        }
    }

    #[test]
    fn plugin_row_status_prefers_active_work_then_failures() {
        let mut state = state_with(Vec::new());
        let plugin = installed_plugin("trae", "0.1.5");
        assert_eq!(plugin_status(&state, &plugin).0, "未检查");

        state.plugin_last_check_ms = 1;
        state.plugin_updates = vec![plugin_entry("trae", "0.1.6")];
        let (status, kind, enabled) = plugin_status(&state, &plugin);
        assert_eq!((status.as_str(), kind, enabled), ("可更新", 2, true));

        // 检查中优先于上次结果。
        state.plugin_checking = true;
        assert_eq!(plugin_status(&state, &plugin).1, 1);

        // 下载中显示百分比，按钮不可再点。
        state.plugin_downloads.insert("trae".into(), (50, 200, 0));
        let (status, kind, enabled) = plugin_status(&state, &plugin);
        assert_eq!((status.as_str(), kind, enabled), ("更新中 25%", 3, false));

        // 导入失败：红色文字，更新按钮置灰；重新检查后才会恢复可更新。
        state.plugin_downloads.clear();
        state.plugin_checking = false;
        state
            .plugin_update_errors
            .insert("trae".into(), "SHA-256 不匹配".into());
        let (status, kind, enabled) = plugin_status(&state, &plugin);
        assert_eq!((status.as_str(), kind, enabled), ("更新失败", 5, false));

        // 更新成功后改为已更新，直到下一次检查。
        state.plugin_update_errors.clear();
        state.plugin_updates.clear();
        state
            .plugin_updated
            .insert("trae".into(), ("0.1.6".into(), true));
        assert_eq!(plugin_status(&state, &plugin).0, "已更新");

        // 新装成功显示「已安装」，与更新区分。
        state
            .plugin_updated
            .insert("trae".into(), ("0.1.6".into(), false));
        assert_eq!(plugin_status(&state, &plugin).0, "已安装");

        // 下载完成、导入中显示安装中。
        state.plugin_updated.clear();
        state.plugin_installing.insert("trae".into());
        let (status, kind, enabled) = plugin_status(&state, &plugin);
        assert_eq!((status.as_str(), kind, enabled), ("安装中…", 3, false));
        state.plugin_installing.clear();

        // 索引检查失败优先于「最新」，不提供行内重试。
        state.plugin_updated.clear();
        state.plugin_check_error = "网络请求失败".into();
        let (status, kind, enabled) = plugin_status(&state, &plugin);
        assert_eq!((status.as_str(), kind, enabled), ("检查失败", 5, false));

        state.plugin_check_error.clear();
        assert_eq!(plugin_status(&state, &plugin).0, "最新");
    }

    #[test]
    fn sidebar_update_state_only_shows_for_known_updates() {
        let mut state = state_with(Vec::new());
        assert_eq!(app_update_sidebar(&state), (0, String::new()));

        // 检查中、检查失败、无新版本都不显示按钮。
        state.app_checking = true;
        assert_eq!(app_update_sidebar(&state), (0, String::new()));
        state.app_checking = false;
        state.app_update_error = "网络请求失败".into();
        assert_eq!(app_update_sidebar(&state), (0, String::new()));
        state.app_update_error.clear();

        // 已确认新版本：显示绿色下载。
        state.app_update = Some(agent_session_manager::update::AppManifest {
            schema: 1,
            version: "0.18.0".into(),
            exe_url: "https://example.com/app.exe".into(),
            sha256: "a".repeat(64),
            size: 100,
            notes_url: String::new(),
        });
        let (kind, tip) = app_update_sidebar(&state);
        assert_eq!(kind, 2);
        assert!(tip.contains("v0.18.0"));

        // 下载中与已下载继续显示。
        state.app_download = Some((50, 100, 1_500_000));
        let (kind, tip) = app_update_sidebar(&state);
        assert_eq!(kind, 4);
        assert!(tip.contains("50%"));
        assert!(tip.contains("1.4 MB/s"));
        state.app_download = None;

        state.app_downloaded = Some(std::path::PathBuf::from(r"C:\tmp\app.exe"));
        assert_eq!(app_update_sidebar(&state).0, 5);
    }

    #[test]
    fn speed_display_uses_unit_matching_magnitude() {
        assert_eq!(format_speed(0), "0 B/s");
        assert_eq!(format_speed(5), "5 B/s");
        assert_eq!(format_speed(1023), "1023 B/s");
        assert_eq!(format_speed(1024), "1.0 KB/s");
        assert_eq!(format_speed(40_592), "39.6 KB/s");
        assert_eq!(format_speed(1024 * 1024), "1.0 MB/s");
        assert_eq!(format_speed(1_500_000), "1.4 MB/s");
    }
}

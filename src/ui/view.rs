//! 把 State 渲染到 Slint 模型：会话列表、安装分组与垃圾会话摘要。

use std::collections::HashMap;
use std::rc::Rc;

use slint::{Model, VecModel};

use agent_session_manager::agents;
use agent_session_manager::ir::SessionSummary;
use agent_session_manager::paths;

use super::assets::Icons;
use super::state::{
    installed_groups, open_defaults, product_visible, selection_summary, session_matches,
    sort_by_workspace, uninstalled_entries, visible_keys, workspace_suggestions_from, State,
};
use super::theme;
use super::{
    fmt_time, AgentFilter, AgentListRow, GarbageSummary, MainWindow, OpenDefaultRow,
    PluginDownloadRow, PluginRow, PluginStoreRow, SessionRow, UpdateLogRow, WorkspaceSuggestion,
};

fn product_icon(state: &State, icons: &Icons, product: &str) -> slint::Image {
    icons.for_product(product, &state.plugins)
}

pub fn apply_view(window: &MainWindow, state: &State, icons: &Icons) {
    let q = state.search.to_lowercase();
    let visible = visible_keys(state);
    let summaries: Vec<SessionSummary> = state
        .all
        .iter()
        .filter(|s| session_matches(state, s, &q))
        .cloned()
        .collect();

    let make_row = |s: &SessionSummary| SessionRow {
        agent: s.agent.clone().into(),
        agent_label: agents::label(&s.agent).into(),
        id: s.id.clone().into(),
        title: s.title.clone().into(),
        cwd: s.cwd.clone().into(),
        updated: fmt_time(s.updated_ms).into(),
        count: s.message_count as i32,
        archived: s.archived,
        deleted: s.deleted,
        workspace_deleted: s.workspace_deleted,
        selected: state.selected.contains(&(s.agent.clone(), s.id.clone())),
        icon: product_icon(state, icons, &s.agent),
        header: false,
        header_label: "".into(),
        header_path: "".into(),
        header_count: 0,
        header_expanded: true,
        header_key: "".into(),
    };

    let rows: Vec<SessionRow> = if state.sort_mode == "workspace" {
        let mut ordered = summaries.clone();
        sort_by_workspace(&mut ordered);
        let mut counts: HashMap<String, usize> = HashMap::new();
        for s in &ordered {
            *counts.entry(paths::key(&s.cwd)).or_default() += 1;
        }
        let mut out = Vec::new();
        let mut last: Option<String> = None;
        for s in &ordered {
            let key = paths::key(&s.cwd);
            if last.as_ref() != Some(&key) {
                out.push(SessionRow {
                    header: true,
                    header_label: paths::name(&s.cwd).into(),
                    header_path: s.cwd.clone().into(),
                    header_count: counts[&key] as i32,
                    header_expanded: !state.workspaces_collapsed.contains(&key),
                    header_key: key.clone().into(),
                    ..make_row(s)
                });
                last = Some(key.clone());
            }
            if state.workspaces_collapsed.contains(&key) {
                continue;
            }
            out.push(make_row(s));
        }
        out
    } else {
        summaries.iter().map(make_row).collect()
    };
    // 原地更新模型内容，而不是替换模型实例：换实例会重置 ListView 滚动位置，
    // 勾选复选框时表现为列表跳到行对齐。
    if let Some(vec_model) = window
        .get_sessions()
        .as_any()
        .downcast_ref::<VecModel<SessionRow>>()
    {
        vec_model.set_vec(rows);
    } else {
        window.set_sessions(Rc::new(VecModel::from(rows)).into());
    }
    // 「全部会话」只统计可见产品的会话；各 agent 计数与总数一次遍历完成。
    let mut agent_counts: HashMap<&str, usize> = HashMap::new();
    let mut count_all = 0usize;
    for s in &state.all {
        if product_visible(state, &s.agent) {
            count_all += 1;
            *agent_counts.entry(s.agent.as_str()).or_default() += 1;
        }
    }
    window.set_count_all(count_all as i32);
    window.set_status_active(state.status_mask & 1 != 0);
    window.set_status_archived(state.status_mask & 2 != 0);
    window.set_status_workspace_deleted(state.status_mask & 4 != 0);
    window.set_status_deleted(state.status_mask & 8 != 0);
    window.set_sort_mode(state.sort_mode.clone().into());
    window.set_session_errors_text(
        super::state::session_issue_summary(&state.session_errors, &state.session_warnings).into(),
    );
    // 建议直接复用本轮已筛选的列表，不再对 state.all 做第二次过滤。
    let (sugg, more) = workspace_suggestions_from(summaries.iter(), 5);
    let sugg: Vec<WorkspaceSuggestion> = sugg
        .into_iter()
        .map(|(name, path)| WorkspaceSuggestion {
            name: name.into(),
            path: path.into(),
        })
        .collect();
    window.set_search_workspaces(Rc::new(VecModel::from(sugg)).into());
    window.set_search_workspaces_more(more);
    window.set_visible_count(visible.len() as i32);
    window.set_all_visible_selected(
        !visible.is_empty() && visible.iter().all(|key| state.selected.contains(key)),
    );
    window.set_selection_count(state.selected.len() as i32);
    window.set_selection_summary(Rc::new(VecModel::from(selection_summary(state, icons))).into());
    let filters: Vec<AgentFilter> = agents::all()
        .iter()
        .filter(|a| product_visible(state, &a.id))
        .map(|a| AgentFilter {
            id: a.id.to_string().into(),
            label: a.label.to_string().into(),
            count: agent_counts.get(a.id.as_ref()).copied().unwrap_or(0) as i32,
            icon: product_icon(state, icons, &a.id),
            selected: state.filter == a.id.as_ref(),
        })
        .collect();
    window.set_filters(Rc::new(VecModel::from(filters)).into());
}

/// 列表行的公共字段：kind 决定样式，其余字段按行类型填。
fn base_row(kind: i32) -> AgentListRow {
    AgentListRow {
        kind,
        text: "".into(),
        product: "".into(),
        variant: "".into(),
        count: 0,
        expanded: true,
        label: "".into(),
        version: "".into(),
        latest: "".into(),
        status: "".into(),
        updatable: false,
        latest_known: false,
        action_enabled: false,
        open_enabled: false,
        can_install: false,
        channel: "".into(),
        icon: Default::default(),
    }
}

/// 插件浮层「下载」页签：下载中显示速度与进度，导入中显示安装状态，
/// 完成后区分「更新完毕」与「安装完毕」。
fn plugin_download_rows(state: &State) -> Vec<PluginDownloadRow> {
    let label_of = |id: &str| {
        state
            .plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .map(|plugin| plugin.label.clone())
            .or_else(|| {
                state
                    .plugin_index
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| {
                        if entry.label.trim().is_empty() {
                            entry.id.clone()
                        } else {
                            entry.label.clone()
                        }
                    })
            })
            .unwrap_or_else(|| id.to_string())
    };
    let version_of = |id: &str| -> String {
        state
            .plugin_updates
            .iter()
            .find(|entry| entry.id == id)
            .or_else(|| state.plugin_index.iter().find(|entry| entry.id == id))
            .map(|entry| entry.version.clone())
            .unwrap_or_default()
    };
    let title_of = |id: &str| -> String {
        let version = version_of(id);
        if version.is_empty() {
            label_of(id)
        } else {
            format!("{} v{version}", label_of(id))
        }
    };
    let mut rows = Vec::new();
    let mut active: Vec<(&String, &(u64, u64, u64))> = state.plugin_downloads.iter().collect();
    active.sort_by(|left, right| left.0.cmp(right.0));
    for (id, (downloaded, total, speed)) in active {
        let progress = if *total == 0 {
            0.0
        } else {
            (*downloaded as f32 / *total as f32).clamp(0.0, 1.0)
        };
        let detail = if *speed == 0 {
            "正在下载…".to_string()
        } else {
            format!("{:.1} MB/s", *speed as f64 / 1_000_000.0)
        };
        rows.push(PluginDownloadRow {
            label: title_of(id).into(),
            detail: detail.into(),
            progress,
            done: false,
        });
    }
    let mut installing: Vec<&String> = state
        .plugin_installing
        .iter()
        .filter(|id| !state.plugin_downloads.contains_key(*id))
        .collect();
    installing.sort();
    for id in installing {
        rows.push(PluginDownloadRow {
            label: title_of(id).into(),
            detail: "下载完毕，正在安装…".into(),
            progress: 1.0,
            done: false,
        });
    }
    let mut finished: Vec<(&String, &(String, bool))> = state.plugin_updated.iter().collect();
    finished.sort_by(|left, right| left.0.cmp(right.0));
    for (id, (version, updated)) in finished {
        if state.plugin_downloads.contains_key(id) || state.plugin_installing.contains(id) {
            continue;
        }
        rows.push(PluginDownloadRow {
            label: format!("{} v{version}", label_of(id)).into(),
            detail: if *updated {
                "更新完毕"
            } else {
                "安装完毕"
            }
            .into(),
            progress: 1.0,
            done: true,
        });
    }
    rows
}

/// 插件库「未安装」列表：来自最近一次成功的插件索引或本地缓存。
fn plugin_store_rows(state: &State) -> Vec<PluginStoreRow> {
    let installed: std::collections::HashSet<&str> = state
        .plugins
        .iter()
        .map(|plugin| plugin.id.as_str())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for entry in &state.plugin_index {
        if installed.contains(entry.id.as_str()) || !seen.insert(entry.id.as_str()) {
            continue;
        }
        let label = if entry.label.trim().is_empty() {
            entry.id.clone()
        } else {
            entry.label.clone()
        };
        let compatible = entry.min_host.trim().is_empty()
            || agent_session_manager::updates::compare(env!("CARGO_PKG_VERSION"), &entry.min_host)
                != std::cmp::Ordering::Less;
        let (installing, progress, status) = if !compatible {
            (false, 0.0, "需要先升级软件".to_string())
        } else {
            match state.plugin_downloads.get(&entry.id) {
                Some((downloaded, total, _)) => {
                    let progress = if *total == 0 {
                        0.0
                    } else {
                        (*downloaded as f32 / *total as f32).clamp(0.0, 1.0)
                    };
                    (true, progress, format!("安装中 {:.0}%", progress * 100.0))
                }
                None => (false, 0.0, "可安装".to_string()),
            }
        };
        rows.push(PluginStoreRow {
            id: entry.id.clone().into(),
            label: label.into(),
            version: entry.version.clone().into(),
            detail: format!("{} · 插件库", entry.id).into(),
            status: status.into(),
            installing,
            installable: compatible,
            progress,
        });
    }
    rows.sort_by(|left, right| left.label.to_lowercase().cmp(&right.label.to_lowercase()));
    rows
}

/// 插件页：已导入插件、在线更新状态与导入入口。
pub fn apply_plugins(window: &MainWindow, state: &State, icons: &Icons) {
    let rows: Vec<PluginRow> = state
        .plugins
        .iter()
        .map(|plugin| {
            let (status, status_kind, update_enabled) = super::state::plugin_status(state, plugin);
            PluginRow {
                id: plugin.id.clone().into(),
                label: plugin.label.clone().into(),
                version: plugin.version.clone().into(),
                root: plugin.root.clone().into(),
                detail: format!(
                    "{} 个形态 · {}",
                    plugin.variants,
                    if plugin.can_write { "可写" } else { "只读" }
                )
                .into(),
                enabled: plugin.enabled,
                status: status.into(),
                status_kind,
                update_enabled,
                icon: icons.for_product(&plugin.id, &state.plugins),
            }
        })
        .collect();
    window.set_plugin_count(rows.len() as i32);
    window.set_plugins_updatable(!state.plugin_updates.is_empty());
    if !state.plugin_tab_initialized.get() {
        if !state.plugins.is_empty() {
            window.set_plugin_tab(0);
            state.plugin_tab_initialized.set(true);
        } else if !state.plugin_index.is_empty() {
            // 没有任何已安装插件时直接展示「未安装」，避免停在空列表。
            window.set_plugin_tab(1);
            state.plugin_tab_initialized.set(true);
        }
    }
    window.set_plugin_importing(state.plugin_importing);
    window.set_plugin_checking(state.plugin_checking);
    let store_rows = plugin_store_rows(state);
    window.set_plugin_store_count(store_rows.len() as i32);
    window.set_plugin_store_error(
        state.plugin_index.is_empty()
            && !state.plugin_checking
            && !state.plugin_check_error.is_empty(),
    );
    window.set_plugin_store_loading(state.plugin_checking && state.plugin_index.is_empty());
    window.set_plugin_store_unchecked(
        state.plugin_last_check_ms == 0
            && state.plugin_index.is_empty()
            && !state.plugin_checking
            && state.plugin_check_error.is_empty(),
    );
    window.set_plugin_store_rows(Rc::new(VecModel::from(store_rows)).into());
    window.set_plugin_last_check(if state.plugin_last_check_ms > 0 {
        agent_session_manager::time::local_time(state.plugin_last_check_ms, "%Y-%m-%d %H:%M").into()
    } else {
        "".into()
    });
    window.set_plugin_log(
        Rc::new(VecModel::from(
            state
                .plugin_log
                .iter()
                .map(|entry| UpdateLogRow {
                    text: entry.text.clone().into(),
                    ok: entry.ok,
                    updatable: entry.updatable,
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    window.set_plugin_downloads(Rc::new(VecModel::from(plugin_download_rows(state))).into());
    // 原地更新模型内容，下载进度只刷新对应行，不重置列表滚动位置。
    if let Some(vec_model) = window
        .get_plugin_rows()
        .as_any()
        .downcast_ref::<VecModel<PluginRow>>()
    {
        vec_model.set_vec(rows);
    } else {
        window.set_plugin_rows(Rc::new(VecModel::from(rows)).into());
    }
}

/// Agent 管理页：已安装与未安装分别铺成两个列表，由页签切换。
pub fn apply_agents(window: &MainWindow, state: &State, icons: &Icons) {
    let groups = installed_groups(state);
    let installed_count: usize = groups.iter().map(|g| g.installed_count).sum();
    let updatable = groups.iter().any(|g| g.rows.iter().any(|r| r.updatable));
    let uninstalled = uninstalled_entries(state);
    let mut installed_rows: Vec<AgentListRow> = vec![base_row(1), base_row(2)];

    for group in groups {
        let icon = product_icon(state, icons, &group.product);
        if group.show_header {
            let mut header = base_row(3);
            header.text = group.label.clone().into();
            header.product = group.product.clone().into();
            header.count = group.installed_count as i32;
            header.expanded = group.expanded;
            installed_rows.push(header);
        }
        if !group.show_header || group.expanded {
            for r in group.rows {
                let mut row = base_row(4);
                row.product = group.product.clone().into();
                row.variant = r.variant.into();
                row.label = r.label.into();
                row.version = r.version.into();
                row.latest = r.latest.into();
                row.status = r.status.into();
                row.updatable = r.updatable;
                row.latest_known = r.latest_known;
                row.action_enabled = r.action_enabled;
                row.open_enabled = r.open_enabled;
                row.channel = r.channel.into();
                row.icon = icon.clone();
                installed_rows.push(row);
            }
        }
    }
    if installed_count == 0 {
        let mut empty = base_row(8);
        empty.text = if state.checking {
            "正在检查…"
        } else {
            "没有已安装的 agent"
        }
        .into();
        installed_rows.push(empty);
    }

    let uninstalled_count = uninstalled.len();
    let mut uninstalled_rows: Vec<AgentListRow> = Vec::new();
    if uninstalled_count == 0 {
        let mut empty = base_row(8);
        empty.text = "没有未安装的 agent".into();
        uninstalled_rows.push(empty);
    } else {
        uninstalled_rows.push(base_row(6));
        for e in uninstalled {
            let mut row = base_row(7);
            row.label = e.label.into();
            row.latest = e.latest.into();
            row.latest_known = e.latest_known;
            row.can_install = e.can_install;
            row.channel = e.channel.into();
            row.icon = product_icon(state, icons, &e.product);
            uninstalled_rows.push(row);
        }
    }

    window.set_agent_installed_rows(Rc::new(VecModel::from(installed_rows)).into());
    window.set_agent_uninstalled_rows(Rc::new(VecModel::from(uninstalled_rows)).into());
    window.set_installed_count(installed_count as i32);
    window.set_uninstalled_count(uninstalled_count as i32);
    window.set_last_check(if state.last_check_ms > 0 {
        agent_session_manager::time::local_time(state.last_check_ms, "%Y-%m-%d %H:%M").into()
    } else {
        "".into()
    });
    window.set_checking(state.checking);
    window.set_update_log(
        Rc::new(VecModel::from(
            state
                .update_log
                .iter()
                .map(|entry| UpdateLogRow {
                    text: entry.text.clone().into(),
                    ok: entry.ok,
                    updatable: entry.updatable,
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    window.set_agents_updatable(updatable);
}

/// 设置页「打开」行与自动检查开关。
pub fn apply_settings(window: &MainWindow, state: &State, icons: &Icons) {
    let rows: Vec<OpenDefaultRow> = open_defaults(state)
        .into_iter()
        .map(|r| OpenDefaultRow {
            product: r.product.clone().into(),
            label: r.label.into(),
            summary: r.summary.into(),
            options: Rc::new(VecModel::from(
                r.options
                    .into_iter()
                    .map(slint::SharedString::from)
                    .collect::<Vec<_>>(),
            ))
            .into(),
            current: r.current.into(),
            icon: product_icon(state, icons, &r.product),
        })
        .collect();
    window.set_open_defaults(Rc::new(VecModel::from(rows)).into());
    window.set_auto_check(state.config.check.auto_on_startup);
    window.set_app_update_button_text(app_update_button_text(state).into());
    window.set_app_update_button_enabled(!state.app_checking && state.app_download.is_none());
    window.set_app_update_detail(app_update_detail(state).into());
    window.set_app_update_progress(app_update_progress(state));
    window.set_app_update_speed(app_update_speed(state).into());
    window.set_app_update_last_check(app_update_last_check(state).into());
    let (sidebar_state, sidebar_tip) = super::state::app_update_sidebar(state);
    window.set_app_update_sidebar_state(sidebar_state);
    window.set_app_update_sidebar_tip(sidebar_tip.into());
    // 主题列表随内置调色板生成，新增主题文件无需改界面。
    window.set_theme_cards(Rc::new(VecModel::from(theme::cards())).into());
    window.set_wallpaper_set(theme::wallpaper_ready(&state.config));
}

fn app_update_button_text(state: &State) -> String {
    if state.app_checking {
        return "检查中…".into();
    }
    if state.app_download.is_some() {
        return "下载中…".into();
    }
    if state.app_downloaded.is_some() {
        return "立即更新".into();
    }
    if let Some(update) = &state.app_update {
        return format!("更新到 v{}", update.version);
    }
    if !state.app_update_error.is_empty() {
        return "重试".into();
    }
    "检查软件更新".into()
}

fn app_update_detail(state: &State) -> String {
    if state.app_checking {
        return "正在检查更新…".into();
    }
    if !state.app_update_error.is_empty() {
        return state.app_update_error.clone();
    }
    if state.app_downloaded.is_some() {
        return "更新已下载，点击按钮替换并重新打开。".into();
    }
    if let Some(update) = &state.app_update {
        return format!("发现新版本 v{}，可更新。", update.version);
    }
    if state.app_last_check_ms > 0 {
        return "当前已是最新版本。".into();
    }
    "启动后与每 20 分钟自动检查软件更新；关闭后可在本页手动检查。".into()
}

fn app_update_progress(state: &State) -> f32 {
    let Some((downloaded, total, _)) = state.app_download else {
        return 0.0;
    };
    if total == 0 {
        return 0.0;
    }
    (downloaded as f32 / total as f32).clamp(0.0, 1.0)
}

fn app_update_speed(state: &State) -> String {
    let Some((_, _, speed)) = state.app_download else {
        return String::new();
    };
    format!("{:.1} MB/s", speed as f64 / 1_000_000.0)
}

fn app_update_last_check(state: &State) -> String {
    if state.app_last_check_ms <= 0 {
        return String::new();
    }
    format!("上次检查 {}", fmt_time(state.app_last_check_ms))
}

pub fn apply_garbage(window: &MainWindow, state: &State, icons: &Icons) {
    let rows: Vec<GarbageSummary> = state
        .garbage
        .iter()
        .map(|(agent, ids)| GarbageSummary {
            agent: agent.clone().into(),
            label: agents::label(agent).into(),
            count: ids.len() as i32,
            icon: product_icon(state, icons, agent),
        })
        .collect();
    let count: i32 = rows.iter().map(|item| item.count).sum();
    window.set_garbage_summary(Rc::new(VecModel::from(rows)).into());
    window.set_garbage_count(count);
    window.set_garbage_scanned(state.garbage_scanned);
    window.set_garbage_scanning(state.garbage_scanning);
}

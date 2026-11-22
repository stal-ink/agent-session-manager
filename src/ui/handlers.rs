//! 窗口与托盘回调绑定；run() 只负责初始化与组装。

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, VecModel};

use agent_session_manager::{agents, export, open, plugins, transfer, update, updates};

use super::events::{show_result, DeleteKind, Event, PendingAction};
use super::shell;
use super::state::{group_by_agent, toggle_select_all, State};
use super::tasks::{
    spawn_app_check, spawn_app_download, spawn_install_scan, spawn_plugin_check,
    spawn_plugin_update, spawn_update_check, spawn_worker, start_refresh,
};
use super::theme;
use super::view::{apply_agents, apply_garbage, apply_plugins, apply_settings, apply_view};
use super::{AgentChoice, Context, MainWindow, Tray};

/// 生成并启动替换 helper；成功后调用方立即退出，由 helper 等待 PID 结束再替换。
fn install_now(new_exe: &std::path::Path) -> anyhow::Result<()> {
    let current = std::env::current_exe()?;
    if !update::install::can_replace(&current) {
        anyhow::bail!("当前程序目录不可写，请手动替换或重新下载安装包");
    }
    let helper =
        update::install::prepare(&transfer::data_dir(), std::process::id(), &current, new_exe)?;
    update::install::spawn(&helper)
}

/// 软件更新入口：设置「关于」与侧栏版本号旁共用同一套状态机。
/// 已下载时打开确认框；有新版本时开始下载；失败或未检查时重新检查。
fn app_update_click(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    icons: &super::assets::Icons,
    queue: &super::events::Queue,
) {
    let mut start_check = false;
    let mut download = None;
    let mut confirm = false;
    {
        let mut st = state.borrow_mut();
        if st.app_checking || st.app_download.is_some() {
            return;
        }
        if st.app_downloaded.is_some() {
            confirm = true;
        } else if !st.app_update_error.is_empty() {
            // 上次检查失败：先重新检查，不下载可能已过期的旧清单。
            st.app_checking = true;
            st.app_update_error.clear();
            start_check = true;
        } else if let Some(manifest) = st.app_update.clone() {
            st.app_download = Some((0, manifest.size, 0));
            download = Some(manifest);
        } else {
            st.app_checking = true;
            start_check = true;
        }
    }
    if start_check {
        spawn_app_check(queue, 0);
    }
    if let Some(manifest) = download {
        spawn_app_download(queue, manifest);
    }
    if confirm {
        let version = state
            .borrow()
            .app_downloaded_version
            .clone()
            .unwrap_or_default();
        w.set_app_update_confirm_version(version.into());
        w.set_app_update_confirm_open(true);
    }
    apply_settings(w, &state.borrow(), icons);
}

pub(super) fn bind(window: &MainWindow, tray: &Tray, ctx: &Context) {
    bind_window(window);
    bind_sessions(window, ctx);
    bind_pages(window, ctx);
    bind_selection(window, ctx);
    bind_transfer(window, ctx);
    bind_results(window, ctx);
    bind_settings(window, ctx);
    bind_tray(tray, ctx);
}

fn bind_window(window: &MainWindow) {
    let weak = window.as_weak();
    window.on_close_window(move || {
        if let Some(window) = weak.upgrade() {
            let _ = window.hide();
        }
    });
}

fn bind_sessions(window: &MainWindow, ctx: &Context) {
    window.on_refresh({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let queue = ctx.queue.clone();
        move || {
            start_refresh(&w.unwrap(), &state, &queue);
        }
    });
    window.on_filter_changed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |f| {
            state.borrow_mut().filter = f.to_string();
            let w = w.unwrap();
            w.set_filter(f);
            w.set_page("sessions".into());
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_search_changed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |s| {
            state.borrow_mut().search = s.to_string();
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_status_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |key, checked| {
            {
                let mut st = state.borrow_mut();
                let bit = match key.as_str() {
                    "active" => 1,
                    "archived" => 2,
                    "workspace" => 4,
                    "deleted" => 8,
                    _ => 0,
                };
                if checked {
                    st.status_mask |= bit;
                } else {
                    st.status_mask &= !bit;
                }
            }
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_sort_changed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |mode| {
            state.borrow_mut().sort_mode = mode.to_string();
            let w = w.unwrap();
            w.set_sort_mode(mode);
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_workspace_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |key| {
            {
                let mut st = state.borrow_mut();
                if !st.workspaces_collapsed.remove(key.as_str()) {
                    st.workspaces_collapsed.insert(key.to_string());
                }
            }
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_selection_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |agent, id, checked| {
            let key = (agent.to_string(), id.to_string());
            {
                let mut st = state.borrow_mut();
                if checked {
                    st.selected.insert(key);
                } else {
                    st.selected.remove(&key);
                }
            }
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_select_all({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move || {
            {
                let mut st = state.borrow_mut();
                toggle_select_all(&mut st);
            }
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
        }
    });
}

fn bind_pages(window: &MainWindow, ctx: &Context) {
    window.on_page_changed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move |page| {
            let w = w.unwrap();
            w.set_page(page.clone());
            match page.as_str() {
                "agents" => {
                    let start = !state.borrow().agents_scanning;
                    if start {
                        state.borrow_mut().agents_scanning = true;
                    }
                    apply_agents(&w, &state.borrow(), &icons);
                    if start {
                        spawn_install_scan(&queue);
                    }
                }
                "plugins" => apply_plugins(&w, &state.borrow(), &icons),
                "more" => apply_garbage(&w, &state.borrow(), &icons),
                _ => {}
            }
        }
    });
    window.on_plugin_import_requested({
        let state = ctx.state.clone();
        let queue = ctx.queue.clone();
        move || {
            if state.borrow().plugin_importing {
                return;
            }
            // 文件选择阶段不改导入状态：取消后界面保持原样，选定文件后才显示「导入中…」。
            spawn_worker(
                &queue,
                || Event::PluginArchivePicked(Err("插件导入选择异常终止".into())),
                move |queue| {
                    let result = theme::pick_plugin_archive().map_err(|error| format!("{error:#}"));
                    queue
                        .lock()
                        .unwrap()
                        .push_back(Event::PluginArchivePicked(result));
                },
            );
        }
    });
    let pending_uninstall: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    window.on_plugin_toggle_requested({
        let queue = ctx.queue.clone();
        move |id, enabled| {
            let id = id.to_string();
            spawn_worker(
                &queue,
                || Event::PluginChanged(Err("插件状态更新异常终止".into())),
                move |queue| {
                    let result = plugins::set_enabled(&id, enabled)
                        .map(|()| String::new())
                        .map_err(|error| format!("{error:#}"));
                    queue
                        .lock()
                        .unwrap()
                        .push_back(Event::PluginChanged(result));
                },
            );
        }
    });
    window.on_plugin_uninstall_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let pending = pending_uninstall.clone();
        move |id| {
            let label = state
                .borrow()
                .plugins
                .iter()
                .find(|plugin| plugin.id.as_str() == id.as_str())
                .map(|plugin| plugin.label.clone())
                .unwrap_or_else(|| id.to_string());
            *pending.borrow_mut() = Some(id.to_string());
            if let Some(w) = w.upgrade() {
                w.set_plugin_uninstall_label(label.into());
                w.set_plugin_uninstall_open(true);
            }
        }
    });
    window.on_plugin_uninstall_confirm({
        let w = ctx.window.clone();
        let pending = pending_uninstall.clone();
        let queue = ctx.queue.clone();
        move || {
            if let Some(w) = w.upgrade() {
                w.set_plugin_uninstall_open(false);
            }
            let Some(id) = pending.borrow_mut().take() else {
                return;
            };
            spawn_worker(
                &queue,
                || Event::PluginChanged(Err("插件卸载异常终止".into())),
                move |queue| {
                    let result = plugins::uninstall(&id)
                        .map(|()| format!("已卸载插件 {id}。插件包已删除，agent 会话数据未改动。"))
                        .map_err(|error| format!("{error:#}"));
                    queue
                        .lock()
                        .unwrap()
                        .push_back(Event::PluginChanged(result));
                },
            );
        }
    });
    window.on_plugin_uninstall_cancel({
        let w = ctx.window.clone();
        move || {
            if let Some(w) = w.upgrade() {
                w.set_plugin_uninstall_open(false);
            }
        }
    });
    window.on_plugin_check_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move || {
            {
                let mut st = state.borrow_mut();
                if st.plugin_checking {
                    return;
                }
                st.plugin_checking = true;
                st.plugin_check_error.clear();
                st.plugin_updated.clear();
            }
            let w = w.unwrap();
            apply_plugins(&w, &state.borrow(), &icons);
            spawn_plugin_check(&queue, 0);
        }
    });
    window.on_plugin_update_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move |id| {
            let id = id.to_string();
            let entry = {
                let mut st = state.borrow_mut();
                if st.plugin_downloads.contains_key(&id) {
                    return;
                }
                // 只有索引确认可更新的插件才允许下载；非可更新状态按钮置灰。
                let Some(entry) = st
                    .plugin_updates
                    .iter()
                    .find(|entry| entry.id == id)
                    .cloned()
                else {
                    return;
                };
                let label = st
                    .plugins
                    .iter()
                    .find(|plugin| plugin.id == id)
                    .map(|plugin| plugin.label.clone())
                    .unwrap_or_else(|| id.clone());
                st.plugin_update_errors.remove(&id);
                st.plugin_updated.remove(&id);
                st.plugin_downloads.insert(id.clone(), (0, entry.size, 0));
                updates::push_log(
                    &mut st.plugin_log,
                    format!("{label} 开始更新到 v{}", entry.version),
                    true,
                );
                entry
            };
            let w = w.unwrap();
            apply_plugins(&w, &state.borrow(), &icons);
            spawn_plugin_update(&queue, entry);
        }
    });
    window.on_plugin_install_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move |id| {
            let id = id.to_string();
            let entry = {
                let mut st = state.borrow_mut();
                if st.plugin_downloads.contains_key(&id) {
                    return;
                }
                let Some(entry) = st.plugin_index.iter().find(|entry| entry.id == id).cloned()
                else {
                    return;
                };
                if !entry.min_host.trim().is_empty()
                    && agent_session_manager::updates::compare(
                        env!("CARGO_PKG_VERSION"),
                        &entry.min_host,
                    ) == std::cmp::Ordering::Less
                {
                    return;
                }
                let label = if entry.label.trim().is_empty() {
                    entry.id.clone()
                } else {
                    entry.label.clone()
                };
                st.plugin_downloads.insert(id.clone(), (0, entry.size, 0));
                updates::push_log(
                    &mut st.plugin_log,
                    format!("开始安装 {label} v{}", entry.version),
                    true,
                );
                entry
            };
            let w = w.unwrap();
            apply_plugins(&w, &state.borrow(), &icons);
            spawn_plugin_update(&queue, entry);
        }
    });
    window.on_check_updates_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move || {
            if state.borrow().checking {
                return;
            }
            state.borrow_mut().checking = true;
            let w = w.unwrap();
            apply_agents(&w, &state.borrow(), &icons);
            // 带上现有缓存与日志：检查失败时保留上一次成功结果，日志实时续写。
            let (prior_cache, prior_log) = {
                let st = state.borrow();
                (
                    updates::UpdateCache {
                        channels: st.updates.clone(),
                        checked_at: st.last_check_ms,
                    },
                    st.update_log.clone(),
                )
            };
            spawn_update_check(&queue, 0, prior_cache, prior_log);
        }
    });
    window.on_agent_download({
        move |channel| {
            let source = updates::load_sources()
                .into_iter()
                .find(|s| s.id == channel.as_str());
            if let Some(source) = source {
                shell::open_url(&source.download_url);
            }
        }
    });
    window.on_agent_open({
        let queue = ctx.queue.clone();
        move |product, variant| {
            let product = product.to_string();
            let variant = variant.to_string();
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("打开 agent".into()),
                move |queue| {
                    if let Err(e) = open::open_variant_direct(&product, &variant) {
                        queue
                            .lock()
                            .unwrap()
                            .push_back(Event::OpenFailed(format!("{e:#}")));
                    }
                },
            );
        }
    });
    window.on_agent_group_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |product| {
            {
                let mut st = state.borrow_mut();
                if !st.groups_collapsed.remove(product.as_str()) {
                    st.groups_collapsed.insert(product.to_string());
                }
            }
            let w = w.unwrap();
            apply_agents(&w, &state.borrow(), &icons);
        }
    });
    window.on_garbage_scan_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move || {
            if state.borrow().garbage_scanning {
                return;
            }
            {
                let mut st = state.borrow_mut();
                st.garbage_scanning = true;
                st.garbage.clear();
                st.garbage_scanned = false;
            }
            let w = w.unwrap();
            apply_garbage(&w, &state.borrow(), &icons);
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("垃圾会话扫描".into()),
                |queue| {
                    let groups = transfer::garbage_groups();
                    queue
                        .lock()
                        .unwrap()
                        .push_back(Event::GarbageScanned { groups });
                },
            );
        }
    });
    window.on_garbage_clear_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let pending = ctx.pending.clone();
        move || {
            let groups = state.borrow().garbage.clone();
            if groups.is_empty() {
                return;
            }
            let count: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
            let detail = groups
                .iter()
                .map(|(agent, ids)| format!("{} {} 条", agents::label(agent), ids.len()))
                .collect::<Vec<_>>()
                .join("，");
            let w = w.unwrap();
            show_result(
                &w,
                "确认清除",
                &format!(
                    "将清除 {count} 条 0 消息会话：{detail}。删除会写入来源存储，确认后不可恢复。"
                ),
                true,
                Some(PendingAction::Delete {
                    groups,
                    kind: DeleteKind::Garbage,
                }),
                &pending,
            );
        }
    });
}

fn bind_selection(window: &MainWindow, ctx: &Context) {
    window.on_selection_open_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let pending = ctx.pending.clone();
        move || {
            let count = state.borrow().selected.len();
            let w = w.unwrap();
            if count == 0 {
                show_result(
                    &w,
                    "未选择会话",
                    "请先勾选要操作的会话。",
                    false,
                    None,
                    &pending,
                );
                return;
            }
            apply_view(&w, &state.borrow(), &icons);
            w.set_selection_open(true);
        }
    });
    window.on_selection_cancel_agent({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |agent| {
            let w = w.unwrap();
            let empty = {
                let mut st = state.borrow_mut();
                st.selected.retain(|(a, _)| a != agent.as_str());
                st.selected.is_empty()
            };
            if empty {
                w.set_selection_open(false);
            }
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_selection_cancel_all({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move || {
            state.borrow_mut().selected.clear();
            let w = w.unwrap();
            w.set_selection_open(false);
            apply_view(&w, &state.borrow(), &icons);
        }
    });
    window.on_selection_delete_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let pending = ctx.pending.clone();
        move || {
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
            let selected: Vec<(String, String)> = state.borrow().selected.iter().cloned().collect();
            if selected.is_empty() {
                w.set_selection_open(false);
                show_result(
                    &w,
                    "未选择会话",
                    "请先勾选要操作的会话。",
                    false,
                    None,
                    &pending,
                );
                return;
            }
            let groups = group_by_agent(selected);
            let count: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
            let detail = groups
                .iter()
                .map(|(agent, ids)| format!("{} {} 条", agents::label(agent), ids.len()))
                .collect::<Vec<_>>()
                .join("，");
            w.set_selection_open(false);
            show_result(
                &w,
                "确认删除",
                &format!("将删除 {count} 条会话：{detail}。删除会写入来源存储，确认后不可恢复。"),
                true,
                Some(PendingAction::Delete {
                    groups,
                    kind: DeleteKind::Selection,
                }),
                &pending,
            );
        }
    });
    window.on_selection_export_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let pending = ctx.pending.clone();
        move || {
            let w = w.unwrap();
            let selected: Vec<(String, String)> = state.borrow().selected.iter().cloned().collect();
            if selected.is_empty() {
                w.set_selection_open(false);
                show_result(
                    &w,
                    "未选择会话",
                    "请先勾选要导出的会话。",
                    false,
                    None,
                    &pending,
                );
                return;
            }
            apply_view(&w, &state.borrow(), &icons);
            // 每次都弹出方式选择，不记忆默认方式。
            w.set_selection_open(false);
            w.set_export_mode_open(true);
        }
    });
    window.on_export_mode_chosen({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let queue = ctx.queue.clone();
        move |mode| {
            let w = w.unwrap();
            let selected: Vec<(String, String)> = state.borrow().selected.iter().cloned().collect();
            let mode = match mode.as_str() {
                "handoff" => export::Mode::Handoff,
                _ => export::Mode::Normal,
            };
            w.set_export_mode_open(false);
            if selected.is_empty() {
                return;
            }
            let groups = group_by_agent(selected);
            // 同一批导出使用同一份配置快照，避免每条会话重复读盘且中途配置漂移。
            let config = agent_session_manager::config::load();
            let options = export::Options {
                normal_tool_results: config.export.normal.tool_results,
                handoff_tool_results: config.export.handoff.tool_results,
            };
            let dir = transfer::data_dir().join("exports");
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("批量导出".into()),
                move |queue| {
                    let mut count = 0usize;
                    let mut failed = Vec::new();
                    for (agent, ids) in &groups {
                        let Some(info) = agents::get(agent) else {
                            failed.push(format!("未知 agent：{agent}"));
                            continue;
                        };
                        for id in ids {
                            match info.adapter.load(id).and_then(|session| {
                                export::export_to(&dir, &session, mode, &options)
                            }) {
                                Ok(_) => count += 1,
                                Err(e) => failed.push(format!("{} {}：{e:#}", info.label, id)),
                            }
                        }
                    }
                    queue
                        .lock()
                        .unwrap()
                        .push_back(Event::ExportFinished { count, dir, failed });
                },
            );
        }
    });
    window.on_destroy_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move || {
            let w = w.unwrap();
            apply_view(&w, &state.borrow(), &icons);
            let selected: Vec<(String, String)> = state.borrow().selected.iter().cloned().collect();
            if selected.is_empty() {
                w.set_selection_open(false);
                return;
            }
            let groups = group_by_agent(selected);
            let count: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
            let summary = groups
                .iter()
                .map(|(agent, ids)| format!("{} {} 条", agents::label(agent), ids.len()))
                .collect::<Vec<_>>()
                .join("，");
            let physical = groups
                .iter()
                .filter(|(agent, _)| {
                    agents::get(agent)
                        .and_then(|info| {
                            info.manifest
                                .as_ref()
                                .map(|manifest| manifest.safety.delete_is_physical)
                        })
                        .unwrap_or(false)
                })
                .map(|(agent, _)| agents::label(agent))
                .collect::<Vec<_>>()
                .join("、");
            let note = if physical.is_empty() {
                "销毁会物理删除会话正文、数据库记录与索引项，执行前会整库备份。".to_string()
            } else {
                format!(
                    "销毁会物理删除会话正文、数据库记录与索引项，执行前会整库备份。{physical} 的删除本就是物理删除，销毁与删除效果相同。"
                )
            };
            w.set_selection_open(false);
            w.set_destroy_summary(
                format!("将销毁 {count} 条会话：{summary}。销毁后无法恢复。").into(),
            );
            w.set_destroy_note(note.into());
            w.set_destroy_open(true);
        }
    });
    window.on_destroy_confirmed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let queue = ctx.queue.clone();
        move || {
            let w = w.unwrap();
            let selected: Vec<(String, String)> = state.borrow().selected.iter().cloned().collect();
            w.set_destroy_open(false);
            if selected.is_empty() {
                return;
            }
            let groups = group_by_agent(selected);
            w.set_loading(true);
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("会话销毁".into()),
                move |queue| {
                    let report = transfer::destroy_groups(&groups);
                    queue.lock().unwrap().push_back(Event::DestroyFinished {
                        report: report.unwrap_or_else(|e| transfer::DestroyReport {
                            lines: vec![format!("{e:#}")],
                            failed: true,
                        }),
                    });
                },
            );
        }
    });
}

fn bind_transfer(window: &MainWindow, ctx: &Context) {
    window.on_transfer_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        move |row| {
            let w = w.unwrap();
            // 只列出本机检测到已安装的可写 agent；扫描缓存为空时不额外收窄。
            let installed: Option<std::collections::HashSet<String>> = {
                let st = state.borrow();
                if st.installs.is_empty() {
                    None
                } else {
                    Some(
                        st.installs
                            .iter()
                            .filter(|install| install.installed)
                            .map(|install| install.id.clone())
                            .collect(),
                    )
                }
            };
            let targets: Vec<AgentChoice> = agents::all()
                .iter()
                .filter(|a| a.can_write && a.id.as_ref() != row.agent.as_str())
                .filter(|a| {
                    installed
                        .as_ref()
                        .map_or(true, |installed| installed.contains(a.id.as_ref()))
                })
                .map(|a| AgentChoice {
                    id: a.id.to_string().into(),
                    label: a.label.to_string().into(),
                })
                .collect();
            let target = targets
                .first()
                .map(|t| t.id.to_string())
                .unwrap_or_default();
            w.set_dialog_targets(std::rc::Rc::new(VecModel::from(targets)).into());
            w.set_dialog_target(target.into());
            w.set_dialog_source(row.clone());
            w.set_dialog_cwd(row.cwd.clone());
            w.set_dialog_native(true);
            w.set_dialog_open(true);
        }
    });
    window.on_dialog_cancel({
        let w = ctx.window.clone();
        move || w.unwrap().set_dialog_open(false)
    });
    window.on_dialog_confirm({
        let w = ctx.window.clone();
        let queue = ctx.queue.clone();
        let pending = ctx.pending.clone();
        move || {
            let w = w.unwrap();
            let src = w.get_dialog_source();
            let cwd = w.get_dialog_cwd().to_string();
            let native = w.get_dialog_native();
            let target = w.get_dialog_target().to_string();
            w.set_dialog_open(false);
            let source = src.agent.to_string();
            if native && target.is_empty() {
                show_result(
                    &w,
                    "无法转接",
                    "没有可写入的目标 agent。",
                    false,
                    None,
                    &pending,
                );
                return;
            }
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("会话转接".into()),
                move |queue| {
                    let req = transfer::Request {
                        source,
                        target: target.clone(),
                        source_id: src.id.to_string(),
                        target_cwd: cwd,
                        mode: if native {
                            transfer::Mode::Native
                        } else {
                            transfer::Mode::Export
                        },
                    };
                    match transfer::execute(&req) {
                        Ok(outcome) => {
                            let native_outcome = matches!(
                                &outcome,
                                transfer::Outcome::Injected { .. }
                                    | transfer::Outcome::Already { .. }
                            );
                            let running = native_outcome
                                && agents::get(&target)
                                    .map(|a| agents::running(&a))
                                    .unwrap_or(false);
                            queue.lock().unwrap().push_back(Event::TransferFinished {
                                result: Ok(outcome),
                                target,
                                target_cwd: req.target_cwd.clone(),
                                running,
                            });
                        }
                        Err(e) => {
                            if let Some(run) = e.downcast_ref::<transfer::TargetRunning>() {
                                queue.lock().unwrap().push_back(Event::TargetBlocked {
                                    target: run.target.clone(),
                                    request: req,
                                });
                            } else {
                                queue.lock().unwrap().push_back(Event::TransferFinished {
                                    result: Err(format!("{e:#}")),
                                    target,
                                    target_cwd: req.target_cwd.clone(),
                                    running: false,
                                });
                            }
                        }
                    }
                },
            );
        }
    });
    window.on_open_requested({
        let queue = ctx.queue.clone();
        move |row| {
            let agent = row.agent.to_string();
            let cwd = row.cwd.to_string();
            let id = row.id.to_string();
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("打开会话".into()),
                move |queue| {
                    if let Err(e) = open::open_in(&agent, &cwd, Some(&id)) {
                        queue
                            .lock()
                            .unwrap()
                            .push_back(Event::OpenFailed(format!("{e:#}")));
                    }
                },
            );
        }
    });
}

fn bind_results(window: &MainWindow, ctx: &Context) {
    window.on_session_issues_details_requested({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let pending = ctx.pending.clone();
        move || {
            let detail = {
                let st = state.borrow();
                super::state::session_issue_details(&st.session_errors, &st.session_warnings)
            };
            if detail.is_empty() {
                return;
            }
            let Some(w) = w.upgrade() else { return };
            show_result(&w, "读取详情", &detail, false, None, &pending);
        }
    });
    window.on_result_confirm({
        let w = ctx.window.clone();
        let pending = ctx.pending.clone();
        let queue = ctx.queue.clone();
        move || {
            let w = w.unwrap();
            w.set_result_open(false);
            let action = pending.borrow_mut().take();
            let Some(action) = action else { return };
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("后台操作".into()),
                move |queue| match action {
                    PendingAction::Open { agent, cwd, id } => {
                        if let Err(e) = open::open_agent_in(&agent, &cwd, Some(&id)) {
                            queue
                                .lock()
                                .unwrap()
                                .push_back(Event::OpenFailed(format!("{e:#}")));
                        }
                    }
                    PendingAction::Restart { agent, cwd, id } => {
                        if let Err(e) = open::restart(&agent, &cwd, Some(&id)) {
                            queue
                                .lock()
                                .unwrap()
                                .push_back(Event::OpenFailed(format!("{e:#}")));
                        }
                    }
                    PendingAction::CloseTransfer(request) => {
                        let target = request.target.clone();
                        if let Err(e) = open::close(&target) {
                            queue
                                .lock()
                                .unwrap()
                                .push_back(Event::OpenFailed(format!("{e:#}")));
                            return;
                        }
                        let result = transfer::execute(&request).map_err(|e| format!("{e:#}"));
                        let reopen_id = match &result {
                            Ok(transfer::Outcome::Injected { target_id })
                            | Ok(transfer::Outcome::Updated { target_id })
                            | Ok(transfer::Outcome::Already { target_id }) => {
                                Some(target_id.clone())
                            }
                            _ => None,
                        };
                        let reopened =
                            open::open_agent_in(&target, &request.target_cwd, reopen_id.as_deref())
                                .map_err(|e| format!("{e:#}"));
                        queue
                            .lock()
                            .unwrap()
                            .push_back(Event::CloseTransferFinished {
                                result,
                                target,
                                reopened,
                            });
                    }
                    PendingAction::Delete { groups, kind } => {
                        let count: usize = groups.iter().map(|(_, ids)| ids.len()).sum();
                        let result = transfer::delete_groups(&groups).map_err(|e| format!("{e:#}"));
                        queue.lock().unwrap().push_back(Event::DeleteFinished {
                            result,
                            count,
                            kind,
                        });
                    }
                },
            );
        }
    });
    window.on_result_cancel({
        let w = ctx.window.clone();
        let pending = ctx.pending.clone();
        move || {
            w.unwrap().set_result_open(false);
            pending.borrow_mut().take();
        }
    });
}

fn bind_settings(window: &MainWindow, ctx: &Context) {
    window.on_settings_search_changed({
        // 搜索命中在 Rust 侧计算：Slint 字符串没有 contains。
        let w = ctx.window.clone();
        move |text| {
            let q = text.to_lowercase();
            let hit = |keywords: &[&str]| {
                q.is_empty() || keywords.iter().any(|k| k.to_lowercase().contains(&q))
            };
            let w = w.unwrap();
            w.set_settings_match_appearance(hit(&["外观", "深色", "浅色", "跟随系统"]));
            w.set_settings_match_export(hit(&[
                "导出与交接",
                "导出工具调用结果",
                "普通文档导出设置",
                "交接文档导出设置",
            ]));
            w.set_settings_match_transfer(hit(&[
                "转接",
                "原生转接设置",
                "仅注入最近压缩之后的消息",
                "压缩",
            ]));
            w.set_settings_match_open(hit(&["打开", "默认打开"]));
            w.set_settings_match_update(hit(&[
                "更新",
                "检查",
                "自动检查更新",
                "软件更新",
                "20 分钟",
            ]));
            w.set_settings_match_data(hit(&["数据与关于", "数据目录", "关于", "应用版本"]));
        }
    });
    window.on_settings_theme_changed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |choice| {
            let w = w.unwrap();
            let dark = {
                let mut st = state.borrow_mut();
                st.config.theme = choice.to_string();
                shell::save_config(&st.config.clone());
                theme::apply(&w, &st.config)
            };
            w.set_theme_choice(choice);
            apply_settings(&w, &state.borrow(), &icons);
            #[cfg(windows)]
            shell::apply_title_bar(dark, 5);
        }
    });
    window.on_settings_wallpaper_choose({
        let queue = ctx.queue.clone();
        move || {
            // 选择框在专用 STA 线程弹出，本任务线程等待结果；界面线程保持可用。
            spawn_worker(
                &queue,
                || Event::BackgroundFailed("选择壁纸".into()),
                move |queue| {
                    let result = match theme::pick_wallpaper() {
                        Ok(Some(path)) => {
                            Some(theme::import_wallpaper(&path).map_err(|e| e.to_string()))
                        }
                        Ok(None) => None,
                        Err(e) => Some(Err(e.to_string())),
                    };
                    if let Some(result) = result {
                        queue
                            .lock()
                            .unwrap()
                            .push_back(Event::WallpaperImported(result));
                    }
                },
            );
        }
    });
    window.on_settings_tool_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        move |kind, value| {
            let w = w.unwrap();
            {
                let mut st = state.borrow_mut();
                match kind.as_str() {
                    "normal" => {
                        st.config.export.normal.tool_results = value;
                        w.set_normal_tool_results(value);
                    }
                    "handoff" => {
                        st.config.export.handoff.tool_results = value;
                        w.set_handoff_tool_results(value);
                    }
                    _ => {}
                }
                shell::save_config(&st.config.clone());
            }
        }
    });
    window.on_settings_compaction_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        move |value| {
            let w = w.unwrap();
            {
                let mut st = state.borrow_mut();
                st.config.transfer.compaction_only = value;
                shell::save_config(&st.config.clone());
            }
            w.set_compaction_only(value);
        }
    });
    window.on_settings_auto_check_toggled({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move |value| {
            let w = w.unwrap();
            let (spawn_agent, spawn_app, spawn_plugin) = {
                let mut st = state.borrow_mut();
                st.config.check.auto_on_startup = value;
                shell::save_config(&st.config.clone());
                if value {
                    let spawn_agent = !st.checking;
                    let spawn_app = !st.app_checking;
                    let spawn_plugin = !st.plugin_checking;
                    st.checking |= spawn_agent;
                    st.app_checking |= spawn_app;
                    st.plugin_checking |= spawn_plugin;
                    (spawn_agent, spawn_app, spawn_plugin)
                } else {
                    (false, false, false)
                }
            };
            w.set_auto_check(value);
            apply_settings(&w, &state.borrow(), &icons);
            apply_plugins(&w, &state.borrow(), &icons);
            if spawn_agent {
                let (cache, log) = {
                    let st = state.borrow();
                    (updates::load_cache(), st.update_log.clone())
                };
                spawn_update_check(&queue, 0, cache, log);
            }
            if spawn_app {
                spawn_app_check(&queue, 0);
            }
            if spawn_plugin {
                spawn_plugin_check(&queue, 0);
            }
        }
    });
    window.on_settings_app_update_clicked({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move || {
            let w = w.unwrap();
            app_update_click(&w, &state, &icons, &queue);
        }
    });
    window.on_app_update_clicked({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let queue = ctx.queue.clone();
        move || {
            let w = w.unwrap();
            app_update_click(&w, &state, &icons, &queue);
        }
    });
    window.on_settings_app_update_confirm({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        let pending = ctx.pending.clone();
        move || {
            let w = w.unwrap();
            w.set_app_update_confirm_open(false);
            let path = state.borrow().app_downloaded.clone();
            let Some(path) = path else { return };
            match install_now(&path) {
                Ok(()) => std::process::exit(0),
                Err(error) => {
                    state.borrow_mut().app_update_error = format!("{error:#}");
                    apply_settings(&w, &state.borrow(), &icons);
                    show_result(&w, "更新失败", &format!("{error:#}"), false, None, &pending);
                }
            }
        }
    });
    window.on_settings_app_update_later({
        let w = ctx.window.clone();
        move || {
            if let Some(w) = w.upgrade() {
                // 保留已下载文件，下次点击「立即更新」直接安装。
                w.set_app_update_confirm_open(false);
            }
        }
    });
    window.on_settings_open_default_changed({
        let w = ctx.window.clone();
        let state = ctx.state.clone();
        let icons = ctx.icons.clone();
        move |product, label| {
            let w = w.unwrap();
            {
                let mut st = state.borrow_mut();
                // 选项只含已安装且可打开的形态，按用户可见名称定位形态 id。
                let variant = st
                    .installs
                    .iter()
                    .find(|i| i.id == product.as_str())
                    .and_then(|i| {
                        i.variants
                            .iter()
                            .find(|v| v.installed && v.kind.label() == label.as_str())
                    })
                    .map(|v| v.id.clone());
                if let Some(variant) = variant {
                    st.config.open.defaults.insert(product.to_string(), variant);
                    shell::save_config(&st.config.clone());
                }
            }
            apply_settings(&w, &state.borrow(), &icons);
        }
    });
    window.on_settings_open_dir({
        move |dir| {
            let base = transfer::data_dir();
            let path = match dir.as_str() {
                "backup" => base.join("backups"),
                "exports" => base.join("exports"),
                // 插件根目录允许在 config.json 中覆盖，优先使用运行时管理器。
                "plugins" => plugins::manager()
                    .map(|manager| manager.root().to_path_buf())
                    .unwrap_or_else(|_| base.join("plugins")),
                _ => base,
            };
            shell::open_path(&path);
        }
    });
}

pub(super) fn show_main_window(w: &MainWindow, dark: bool) {
    w.window().set_minimized(false);
    w.show().ok();
    #[cfg(windows)]
    {
        shell::bring_to_front();
        shell::apply_title_bar(dark, 5);
    }
    #[cfg(not(windows))]
    let _ = dark;
}

fn bind_tray(tray: &Tray, ctx: &Context) {
    tray.on_show_window({
        let w = ctx.window.clone();
        let dark = ctx.dark;
        move || {
            show_main_window(&w.unwrap(), dark);
        }
    });
    tray.on_quit(|| {
        slint::quit_event_loop().ok();
    });
}

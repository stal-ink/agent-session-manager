//! UI 事件泵：每 150ms 取走后台队列中的事件并更新窗口。

use std::collections::HashSet;

use slint::{ComponentHandle, Model};

use agent_session_manager::{agents, plugins, transfer};

use super::events::{notice_transferred, show_result, DeleteKind, Event};
use super::state::{ensure_filter_visible, refresh_is_current};
use super::tasks::{finish_refresh, spawn_install_scan, spawn_worker, start_refresh};
use super::view::{apply_agents, apply_garbage, apply_plugins, apply_settings, apply_view};
use super::Context;

/// 启动事件泵；返回的 Timer 必须由调用方持有，否则定时器随对象析构停止。
pub(super) fn start(ctx: &Context) -> slint::Timer {
    let pump = slint::Timer::default();
    pump.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(150),
        {
            let weak = ctx.window.clone();
            let state = ctx.state.clone();
            let icons = ctx.icons.clone();
            let queue = ctx.queue.clone();
            let refresh_queue = ctx.queue.clone();
            let pending = ctx.pending.clone();
            let dark = ctx.dark;
            move || {
                let events: Vec<Event> = queue.lock().unwrap().drain(..).collect();
                let Some(w) = weak.upgrade() else { return };
                #[cfg(windows)]
                {
                    super::dpi::ensure(w.window(), (0.0, 0.0));
                    let weak = w.as_weak();
                    super::chrome::ensure(w.window(), move |x, y, width| {
                        let Some(w) = weak.upgrade() else { return };
                        if w.get_settings_open() || w.get_page().as_str() != "sessions" {
                            w.set_search_open(false);
                            return;
                        }
                        // 与 main.slint 的搜索框/浮层几何保持一致：搜索框 250×30，
                        // 右侧 542px（16 内边距 + 3×80 按钮 + 3×12 间距 + 250）。
                        let left = width - 542;
                        let in_search = x >= left && x <= left + 250 && y >= 58 && y <= 88;
                        let rows = w.get_search_workspaces().row_count()
                            + usize::from(w.get_search_workspaces_more());
                        let bottom = 92 + 8 + 34 * i32::try_from(rows).unwrap_or(6);
                        let in_dropdown = w.get_search_open()
                            && x >= left
                            && x <= left + 250
                            && y >= 92
                            && y <= bottom;
                        if in_search {
                            w.set_search_open(true);
                        } else if !in_dropdown {
                            w.set_search_open(false);
                            let token = w.get_dismiss_search_token();
                            w.set_dismiss_search_token(token.wrapping_add(1));
                        }
                    });
                }
                for e in events {
                    match e {
                        Event::Loaded {
                            sessions,
                            open_session_ids,
                            errors,
                            warnings,
                            generation,
                        } => {
                            // 过期代次直接丢弃，避免旧结果覆盖新结果或提前收起 loading。
                            if !refresh_is_current(&state.borrow(), generation) {
                                continue;
                            }
                            let n = sessions.len();
                            let failed = errors.len();
                            let error_text: Vec<String> = errors
                                .into_iter()
                                .map(|e| {
                                    if e.message.is_empty() {
                                        e.label
                                    } else {
                                        format!("{}（{}）", e.label, e.message)
                                    }
                                })
                                .collect();
                            let warning_text: Vec<String> = warnings
                                .into_iter()
                                .map(|e| {
                                    if e.message.is_empty() {
                                        e.label
                                    } else {
                                        format!("{}（{}）", e.label, e.message)
                                    }
                                })
                                .collect();
                            let existing: HashSet<(String, String)> = sessions
                                .iter()
                                .map(|s| (s.agent.clone(), s.id.clone()))
                                .collect();
                            {
                                let mut st = state.borrow_mut();
                                st.selected.retain(|key| existing.contains(key));
                                st.all = sessions;
                                super::state::apply_open_ids_archive(
                                    &mut st.all,
                                    open_session_ids.as_ref(),
                                );
                                super::state::sort_sessions(&mut st.all);
                                st.session_errors = error_text;
                                st.session_warnings = warning_text;
                            }
                            apply_view(&w, &state.borrow(), &icons);
                            w.set_status_text(
                                if failed == 0 {
                                    format!("已加载 {n} 个会话")
                                } else {
                                    format!("已加载 {n} 个会话，{failed} 个 agent 读取失败")
                                }
                                .into(),
                            );
                            finish_refresh(&w, &state, &refresh_queue);
                        }
                        Event::RefreshFailed { generation, what } => {
                            if !refresh_is_current(&state.borrow(), generation) {
                                continue;
                            }
                            finish_refresh(&w, &state, &refresh_queue);
                            show_result(
                                &w,
                                "后台任务失败",
                                &format!("{what}异常终止，请重试。"),
                                false,
                                None,
                                &pending,
                            );
                        }
                        Event::TransferFinished {
                            result,
                            target,
                            target_cwd,
                            running,
                        } => match result {
                            Ok(transfer::Outcome::Exported { path }) => show_result(
                                &w,
                                "导出成功",
                                &format!("已导出到：{}", path.display()),
                                false,
                                None,
                                &pending,
                            ),
                            Ok(transfer::Outcome::Injected { target_id }) => {
                                start_refresh(&w, &state, &refresh_queue);
                                notice_transferred(
                                    &w,
                                    &target,
                                    &target_cwd,
                                    &target_id,
                                    running,
                                    "转接成功",
                                    "",
                                    &pending,
                                );
                            }
                            Ok(transfer::Outcome::Updated { target_id }) => {
                                start_refresh(&w, &state, &refresh_queue);
                                notice_transferred(
                                    &w,
                                    &target,
                                    &target_cwd,
                                    &target_id,
                                    running,
                                    "转接成功",
                                    "目标端旧副本已覆盖。",
                                    &pending,
                                );
                            }
                            Ok(transfer::Outcome::Already { target_id }) => {
                                start_refresh(&w, &state, &refresh_queue);
                                notice_transferred(
                                    &w,
                                    &target,
                                    &target_cwd,
                                    &target_id,
                                    running,
                                    "已转接过",
                                    "该会话自上次转接后没有更新。",
                                    &pending,
                                );
                            }
                            Err(e) => show_result(&w, "转接失败", &e, false, None, &pending),
                        },
                        Event::OpenFailed(e) => {
                            show_result(&w, "打开失败", &e, false, None, &pending)
                        }
                        Event::ShowWindow => super::handlers::show_main_window(&w, dark),
                        Event::WallpaperImported(result) => match result {
                            Ok(name) => {
                                {
                                    let mut st = state.borrow_mut();
                                    st.config.wallpaper = name;
                                    super::shell::save_config(&st.config.clone());
                                    super::theme::apply(&w, &st.config);
                                }
                                apply_settings(&w, &state.borrow(), &icons);
                            }
                            Err(e) => {
                                show_result(&w, "壁纸导入失败", &e, false, None, &pending)
                            }
                        },
                        Event::PluginArchivePicked(result) => match result {
                            Ok(Some(path)) => {
                                state.borrow_mut().plugin_importing = true;
                                apply_plugins(&w, &state.borrow(), &icons);
                                spawn_worker(
                                    &refresh_queue,
                                    || {
                                        Event::PluginImportFinished(Err(
                                            "插件导入任务异常终止".into(),
                                        ))
                                    },
                                    move |queue| {
                                        let result = (|| -> anyhow::Result<String> {
                                            let imported = plugins::import_archive(&path)?;
                                            let label = imported.info.label.to_string();
                                            agents::upsert_external(imported.info)?;
                                            Ok(label)
                                        })()
                                        .map_err(|error| format!("{error:#}"));
                                        queue
                                            .lock()
                                            .unwrap()
                                            .push_back(Event::PluginImportFinished(result));
                                    },
                                );
                            }
                            Ok(None) => {}
                            Err(error) => show_result(
                                &w,
                                "插件导入失败",
                                &error,
                                false,
                                None,
                                &pending,
                            ),
                        },
                        Event::PluginImportFinished(result) => {
                            let mut st = state.borrow_mut();
                            st.plugin_importing = false;
                            match result {
                                Ok(label) => {
                                    st.plugins =
                                        agent_session_manager::plugins::installed().unwrap_or_default();
                                    st.catalog = agent_session_manager::catalog::from_plugins();
                                    st.sources = agent_session_manager::updates::load_sources();
                                    st.agents_scanning = true;
                                    ensure_filter_visible(&mut st);
                                    let filter = st.filter.clone();
                                    drop(st);
                                    w.set_filter(filter.into());
                                    apply_plugins(&w, &state.borrow(), &icons);
                                    apply_agents(&w, &state.borrow(), &icons);
                                    apply_settings(&w, &state.borrow(), &icons);
                                    apply_view(&w, &state.borrow(), &icons);
                                    spawn_install_scan(&refresh_queue);
                                    start_refresh(&w, &state, &refresh_queue);
                                    if !label.is_empty() {
                                        show_result(
                                            &w,
                                            "插件导入完成",
                                            &format!(
                                                "已导入 {label}。Agent 管理页已按插件声明的形态刷新。"
                                            ),
                                            false,
                                            None,
                                            &pending,
                                        );
                                    }
                                }
                                Err(error) => {
                                    drop(st);
                                    apply_plugins(&w, &state.borrow(), &icons);
                                    show_result(
                                        &w,
                                        "插件导入失败",
                                        &error,
                                        false,
                                        None,
                                        &pending,
                                    );
                                }
                            }
                        }
                        Event::PluginChanged(result) => {
                            let mut st = state.borrow_mut();
                            match result {
                                Ok(message) => {
                                    st.plugins =
                                        agent_session_manager::plugins::installed().unwrap_or_default();
                                    st.catalog = agent_session_manager::catalog::from_plugins();
                                    st.sources = agent_session_manager::updates::load_sources();
                                    st.agents_scanning = true;
                                    ensure_filter_visible(&mut st);
                                    let filter = st.filter.clone();
                                    drop(st);
                                    w.set_filter(filter.into());
                                    apply_plugins(&w, &state.borrow(), &icons);
                                    apply_agents(&w, &state.borrow(), &icons);
                                    apply_settings(&w, &state.borrow(), &icons);
                                    apply_view(&w, &state.borrow(), &icons);
                                    spawn_install_scan(&refresh_queue);
                                    start_refresh(&w, &state, &refresh_queue);
                                    if !message.is_empty() {
                                        show_result(
                                            &w,
                                            "插件已更新",
                                            &message,
                                            false,
                                            None,
                                            &pending,
                                        );
                                    }
                                }
                                Err(error) => {
                                    drop(st);
                                    apply_plugins(&w, &state.borrow(), &icons);
                                    show_result(
                                        &w,
                                        "插件操作失败",
                                        &error,
                                        false,
                                        None,
                                        &pending,
                                    );
                                }
                            }
                        }
                        Event::BackgroundFailed(what) => {
                            // panic 可能发生在任务开始之后，这里复位避免按钮或插件行卡在进行中。
                            {
                                let mut st = state.borrow_mut();
                                st.checking = false;
                                st.app_checking = false;
                                st.plugin_checking = false;
                                st.plugin_downloads.clear();
                                st.app_download = None;
                            }
                            w.set_loading(false);
                            apply_agents(&w, &state.borrow(), &icons);
                            apply_plugins(&w, &state.borrow(), &icons);
                            apply_settings(&w, &state.borrow(), &icons);
                            show_result(
                                &w,
                                "后台任务失败",
                                &format!("{what}异常终止，请重试。"),
                                false,
                                None,
                                &pending,
                            )
                        }
                        Event::TargetBlocked { target, request } => show_result(
                            &w,
                            "目标应用正在运行",
                            &format!(
                                "{} 正在运行。请在 5 秒内关闭 {}，否则将强制结束进程，未保存的内容可能丢失。关闭后完成转接并重新打开 {}。是否继续？",
                                agents::label(&target),
                                agents::label(&target),
                                agents::label(&target)
                            ),
                            true,
                            Some(super::events::PendingAction::CloseTransfer(request)),
                            &pending,
                        ),
                        Event::CloseTransferFinished {
                            result,
                            target,
                            reopened,
                        } => match result {
                            Ok(_) => {
                                start_refresh(&w, &state, &refresh_queue);
                                let message = match reopened {
                                    Ok(()) => {
                                        format!("转接完成，{} 已重新打开。", agents::label(&target))
                                    }
                                    Err(e) => format!(
                                        "转接完成，但重新打开 {} 失败：{e}",
                                        agents::label(&target)
                                    ),
                                };
                                show_result(&w, "转接成功", &message, false, None, &pending);
                            }
                            Err(e) => {
                                let mut message = e;
                                if let Err(re) = reopened {
                                    message.push_str(&format!(
                                        "；重新打开 {} 失败：{re}",
                                        agents::label(&target)
                                    ));
                                }
                                show_result(&w, "转接失败", &message, false, None, &pending);
                            }
                        },
                        Event::DeleteFinished {
                            result,
                            count,
                            kind,
                        } => match result {
                            Ok(()) => {
                                {
                                    let mut st = state.borrow_mut();
                                    match kind {
                                        DeleteKind::Selection => st.selected.clear(),
                                        DeleteKind::Garbage => {
                                            st.garbage.clear();
                                            st.garbage_scanned = false;
                                        }
                                    }
                                }
                                apply_view(&w, &state.borrow(), &icons);
                                apply_garbage(&w, &state.borrow(), &icons);
                                start_refresh(&w, &state, &refresh_queue);
                                let (title, message) = match kind {
                                    DeleteKind::Selection => {
                                        ("删除完成".to_string(), format!("已删除 {count} 条会话。"))
                                    }
                                    DeleteKind::Garbage => (
                                        "清除完成".to_string(),
                                        format!("已清除 {count} 条 0 消息会话。"),
                                    ),
                                };
                                show_result(&w, &title, &message, false, None, &pending);
                            }
                            Err(e) => {
                                let title = match kind {
                                    DeleteKind::Selection => "删除失败",
                                    DeleteKind::Garbage => "清除失败",
                                };
                                show_result(&w, title, &e, false, None, &pending);
                            }
                        },
                        Event::ExportFinished {
                            count,
                            dir,
                            failed,
                        } => {
                            let (title, message) = if count == 0 {
                                (
                                    "导出失败",
                                    if failed.is_empty() {
                                        "没有可导出的会话。".to_string()
                                    } else {
                                        failed.join("；")
                                    },
                                )
                            } else if failed.is_empty() {
                                (
                                    "导出完成",
                                    format!("已导出 {count} 条会话到：{}", dir.display()),
                                )
                            } else {
                                (
                                    "导出完成",
                                    format!(
                                        "已导出 {count} 条会话到：{}；{} 条失败：{}",
                                        dir.display(),
                                        failed.len(),
                                        failed.join("；")
                                    ),
                                )
                            };
                            show_result(&w, title, &message, false, None, &pending);
                        }
                        Event::InstallsScanned(list) => {                            let filter = {
                                let mut st = state.borrow_mut();
                                st.installs = list;
                                st.catalog = agent_session_manager::catalog::from_plugins();
                                st.sources = agent_session_manager::updates::load_sources();
                                st.agents_scanning = false;
                                // 扫描后重算可见产品；当前筛选被隐藏时回退「全部会话」。
                                ensure_filter_visible(&mut st);
                                st.filter.clone()
                            };
                            w.set_filter(filter.into());
                            apply_view(&w, &state.borrow(), &icons);
                            // 安装扫描会更新本机版本号，Agent 管理行同步刷新。
                            apply_agents(&w, &state.borrow(), &icons);
                            apply_settings(&w, &state.borrow(), &icons);
                        }
                        Event::UpdateCheckProgress { updates, log } => {
                            let mut st = state.borrow_mut();
                            st.updates = updates;
                            st.update_log = log;
                            apply_agents(&w, &st, &icons);
                        }
                        Event::UpdateCheckFinished {
                            updates,
                            log,
                            checked_at,
                        } => {
                            let mut st = state.borrow_mut();
                            st.updates = updates;
                            st.update_log = log;
                            st.checking = false;
                            st.last_check_ms = checked_at;
                            apply_agents(&w, &st, &icons);
                        }
                        Event::AppUpdateChecked {
                            manifest,
                            error,
                            checked_at,
                        } => {
                            let mut st = state.borrow_mut();
                            st.app_checking = false;
                            st.app_last_check_ms = checked_at;
                            if error.is_empty() {
                                st.app_update_error.clear();
                                match &manifest {
                                    Some(update)
                                        if st.app_downloaded_version.as_deref()
                                            == Some(update.version.as_str()) =>
                                    {
                                        st.app_update = manifest;
                                    }
                                    Some(_) | None => {
                                        // 远端版本已变化或已是最新：旧下载不再可用。
                                        st.app_update = manifest;
                                        st.app_downloaded = None;
                                        st.app_downloaded_version = None;
                                    }
                                }
                            } else {
                                // 失败保留上次成功结果与已下载文件，只记录本次错误。
                                st.app_update_error = error;
                            }
                            apply_settings(&w, &st, &icons);
                        }
                        Event::PluginIndexChecked {
                            updates,
                            plugins,
                            error,
                            checked_at,
                        } => {
                            let mut st = state.borrow_mut();
                            st.plugin_checking = false;
                            st.plugin_last_check_ms = checked_at;
                            if let Some(plugins) = plugins {
                                st.plugin_index = plugins;
                            }
                            if error.is_empty() {
                                st.plugin_check_error.clear();
                                st.plugin_update_errors.clear();
                                // 与 Agent 管理页一致：逐个插件写结果，再写一条汇总。
                                let plugins: Vec<(String, String)> = st
                                    .plugins
                                    .iter()
                                    .map(|plugin| (plugin.id.clone(), plugin.label.clone()))
                                    .collect();
                                for (id, label) in &plugins {
                                    if let Some(entry) =
                                        updates.iter().find(|entry| entry.id == *id)
                                    {
                                        agent_session_manager::updates::push_log_entry(
                                            &mut st.plugin_log,
                                            format!("{label} 可更新到 v{}", entry.version),
                                            true,
                                            true,
                                        );
                                    } else {
                                        agent_session_manager::updates::push_log(
                                            &mut st.plugin_log,
                                            format!("{label} 已是最新"),
                                            true,
                                        );
                                    }
                                }
                                agent_session_manager::updates::push_log(
                                    &mut st.plugin_log,
                                    if updates.is_empty() {
                                        "检查完成：全部插件已是最新".to_string()
                                    } else {
                                        format!("检查完成：{} 个插件可更新", updates.len())
                                    },
                                    true,
                                );
                            } else {
                                // 失败的是整个插件索引，不是单个插件。
                                agent_session_manager::updates::push_log(
                                    &mut st.plugin_log,
                                    "插件索引获取失败",
                                    false,
                                );
                                st.plugin_check_error = error;
                            }
                            st.plugin_updates = updates;
                            st.plugin_updated.clear();
                            apply_plugins(&w, &st, &icons);
                        }
                        Event::PluginUpdateProgress {
                            id,
                            downloaded,
                            total,
                            speed_bps,
                        } => {
                            let mut st = state.borrow_mut();
                            st.plugin_downloads
                                .insert(id, (downloaded, total, speed_bps));
                            apply_plugins(&w, &st, &icons);
                        }
                        Event::PluginUpdateDownloaded { id } => {
                            let mut st = state.borrow_mut();
                            st.plugin_downloads.remove(&id);
                            st.plugin_installing.insert(id);
                            apply_plugins(&w, &st, &icons);
                        }
                        Event::PluginUpdateFinished {
                            id,
                            version,
                            result,
                        } => {
                            let mut st = state.borrow_mut();
                            if id.is_empty() {
                                st.plugin_downloads.clear();
                                st.plugin_installing.clear();
                                agent_session_manager::updates::push_log(
                                    &mut st.plugin_log,
                                    "插件更新任务异常终止",
                                    false,
                                );
                                drop(st);
                                apply_plugins(&w, &state.borrow(), &icons);
                                show_result(
                                    &w,
                                    "插件更新失败",
                                    "插件更新任务异常终止，请重试。",
                                    false,
                                    None,
                                    &pending,
                                );
                                continue;
                            }
                            let label = st
                                .plugins
                                .iter()
                                .find(|plugin| plugin.id == id)
                                .map(|plugin| plugin.label.clone())
                                .unwrap_or_else(|| id.clone());
                            let installed_before =
                                st.plugins.iter().any(|plugin| plugin.id == id);
                            st.plugin_downloads.remove(&id);
                            st.plugin_installing.remove(&id);
                            match result {
                                Ok(_label) => {
                                    let message = if installed_before {
                                        format!("{label} 已更新到 v{version}")
                                    } else {
                                        format!("{label} 已安装 v{version}")
                                    };
                                    agent_session_manager::updates::push_log_entry(
                                        &mut st.plugin_log,
                                        message,
                                        true,
                                        true,
                                    );
                                    st.plugin_updates.retain(|entry| entry.id != id);
                                    st.plugin_update_errors.remove(&id);
                                    st.plugin_updated
                                        .insert(id.clone(), (version.clone(), installed_before));
                                    st.plugins = plugins::installed().unwrap_or_default();
                                    st.catalog = agent_session_manager::catalog::from_plugins();
                                    st.sources = agent_session_manager::updates::load_sources();
                                    st.agents_scanning = true;
                                    ensure_filter_visible(&mut st);
                                    let filter = st.filter.clone();
                                    drop(st);
                                    w.set_filter(filter.into());
                                    apply_plugins(&w, &state.borrow(), &icons);
                                    apply_agents(&w, &state.borrow(), &icons);
                                    apply_settings(&w, &state.borrow(), &icons);
                                    apply_view(&w, &state.borrow(), &icons);
                                    spawn_install_scan(&refresh_queue);
                                    start_refresh(&w, &state, &refresh_queue);
                                }
                                Err(error) => {
                                    agent_session_manager::updates::push_log(
                                        &mut st.plugin_log,
                                        format!("{label} 更新失败：{error}"),
                                        false,
                                    );
                                    st.plugin_update_errors.insert(id.clone(), error);
                                    drop(st);
                                    apply_plugins(&w, &state.borrow(), &icons);
                                }
                            }
                        }
                        Event::AppUpdateProgress {
                            downloaded,
                            total,
                            speed_bps,
                        } => {
                            let mut st = state.borrow_mut();
                            st.app_download = Some((downloaded, total, speed_bps));
                            apply_settings(&w, &st, &icons);
                        }
                        Event::AppUpdateDownloaded { result, version } => {
                            let mut st = state.borrow_mut();
                            st.app_download = None;
                            match result {
                                Ok(path) => {
                                    st.app_downloaded = Some(path);
                                    st.app_downloaded_version = Some(version);
                                    st.app_update_error.clear();
                                }
                                Err(error) => st.app_update_error = error,
                            }
                            apply_settings(&w, &st, &icons);
                        }
                        Event::DestroyFinished { report } => {
                            {
                                let mut st = state.borrow_mut();
                                st.selected.clear();
                            }
                            apply_view(&w, &state.borrow(), &icons);
                            start_refresh(&w, &state, &refresh_queue);
                            let (title, message) = if report.failed {
                                (
                                    "销毁完成（部分失败）".to_string(),
                                    report.lines.join("\n"),
                                )
                            } else {
                                ("销毁完成".to_string(), report.lines.join("\n"))
                            };
                            show_result(&w, &title, &message, false, None, &pending);
                        }
                        Event::GarbageScanned { groups } => {
                            let mut st = state.borrow_mut();
                            st.garbage = groups;
                            st.garbage_scanned = true;
                            st.garbage_scanning = false;
                            apply_garbage(&w, &st, &icons);
                        }
                    }
                }
            }
        },
    );
    pump
}

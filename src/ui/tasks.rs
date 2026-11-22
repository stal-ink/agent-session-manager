//! 后台任务：会话刷新、安装扫描，以及线程 panic 时的兜底事件。

use std::cell::RefCell;
use std::rc::Rc;

use agent_session_manager::{agents, installs, plugins, update, updates};

use super::events::{AgentReadError, AgentReadWarning, Event, Queue};
use super::state::sort_sessions;
use super::state::State;
use super::MainWindow;

/// 启动后台任务：线程 panic 时向队列投递兜底事件，避免 UI 永久停在 loading。
pub(super) fn spawn_worker(
    queue: &Queue,
    fallback: impl FnOnce() -> Event + Send + 'static,
    body: impl FnOnce(Queue) + Send + 'static,
) {
    let fallback_queue = queue.clone();
    let worker_queue = queue.clone();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            body(worker_queue);
        }));
        if outcome.is_err() {
            fallback_queue.lock().unwrap().push_back(fallback());
        }
    });
}

pub(super) fn spawn_install_scan(queue: &Queue) {
    spawn_worker(
        queue,
        || Event::BackgroundFailed("agent 安装扫描".into()),
        |queue| {
            let list = installs::scan();
            installs::save_cache(&list);
            queue
                .lock()
                .unwrap()
                .push_back(Event::InstallsScanned(list));
        },
    );
}

/// 版本检查：单线程依次检查已安装形态与未安装条目，网络请求在后台线程执行。
/// 结果缓存（updates.json）原地更新，日志只在内存实时追加；
/// prior 是发起检查时的现有缓存与日志。delay_secs > 0 用于启动后延迟一次的自动检查。
pub(super) fn spawn_update_check(
    queue: &Queue,
    delay_secs: u64,
    prior_cache: updates::UpdateCache,
    prior_log: Vec<updates::LogEntry>,
) {
    spawn_worker(
        queue,
        || Event::BackgroundFailed("版本检查".into()),
        move |queue| {
            if delay_secs > 0 {
                std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            }
            agent_check_once(&queue, prior_cache, prior_log);
        },
    );
}

/// App 自更新清单检查；延迟用于启动后的自动检查。
pub(super) fn spawn_app_check(queue: &Queue, delay_secs: u64) {
    spawn_worker(
        queue,
        || Event::BackgroundFailed("软件更新检查".into()),
        move |queue| {
            if delay_secs > 0 {
                std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            }
            app_check_once(&queue);
        },
    );
}

/// 插件索引检查：读取已安装插件版本，只挑出可更新且满足 min_host 的条目。
pub(super) fn spawn_plugin_check(queue: &Queue, delay_secs: u64) {
    spawn_worker(
        queue,
        || Event::BackgroundFailed("插件更新检查".into()),
        move |queue| {
            if delay_secs > 0 {
                std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            }
            plugin_check_once(&queue);
        },
    );
}

/// 自动 tick 按 app → 插件 → agent 串行检查，避免同一时刻发起多组网络请求。
pub(super) fn spawn_auto_checks(
    queue: &Queue,
    delay_secs: u64,
    prior_cache: updates::UpdateCache,
    prior_log: Vec<updates::LogEntry>,
) {
    spawn_worker(
        queue,
        || Event::BackgroundFailed("自动更新检查".into()),
        move |queue| {
            if delay_secs > 0 {
                std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            }
            app_check_once(&queue);
            plugin_check_once(&queue);
            agent_check_once(&queue, prior_cache, prior_log);
        },
    );
}

fn app_check_once(queue: &Queue) {
    let (manifest, error) = match update::check_app(&update::app_manifest_url()) {
        Ok(manifest) => (manifest, String::new()),
        Err(error) => (None, format!("{error:#}")),
    };
    queue.lock().unwrap().push_back(Event::AppUpdateChecked {
        manifest,
        error,
        checked_at: agent_session_manager::ir::now_ms(),
    });
}

fn plugin_check_once(queue: &Queue) {
    let index_url = update::plugin_index_url();
    let (updates, plugins, error) = match update::check_plugins(&index_url) {
        Ok(index) => {
            let installed: std::collections::HashMap<String, String> = plugins::installed()
                .unwrap_or_default()
                .into_iter()
                .map(|plugin| (plugin.id, plugin.version))
                .collect();
            // 缓存保存原始索引：相对 zip_url 不绑定当前 hub，换地址后仍可解析。
            let _ = update::plugins::save_index_cache(
                &agent_session_manager::transfer::data_dir(),
                &index,
            );
            let mut resolved = index;
            match update::resolve_plugin_index(&index_url, &mut resolved) {
                Ok(()) => {
                    let updates = update::select_plugin_updates(&installed, &resolved);
                    (updates, Some(resolved.plugins), String::new())
                }
                Err(error) => (Vec::new(), None, format!("{error:#}")),
            }
        }
        Err(error) => (Vec::new(), None, format!("{error:#}")),
    };
    queue.lock().unwrap().push_back(Event::PluginIndexChecked {
        updates,
        plugins,
        error,
        checked_at: agent_session_manager::ir::now_ms(),
    });
}

fn agent_check_once(
    queue: &Queue,
    prior_cache: updates::UpdateCache,
    prior_log: Vec<updates::LogEntry>,
) {
    let installs = installs::load_cache();
    let mut targets = Vec::new();
    for installed in [true, false] {
        for item in &installs {
            for variant in &item.variants {
                if variant.installed != installed {
                    continue;
                }
                targets.push(updates::CheckTarget {
                    channel: variant.channel.clone(),
                    label: variant.kind.label().to_string(),
                    installed: variant.installed,
                    version: variant.version.clone(),
                });
            }
        }
    }
    // 没有可检查目标也要回执，否则 checking 状态卡住。
    let mut cache = prior_cache;
    let mut log = prior_log;
    let updates = if targets.is_empty() {
        Vec::new()
    } else {
        updates::check_with(&mut cache, &mut log, &targets, |cache, log| {
            queue.lock().unwrap().push_back(Event::UpdateCheckProgress {
                updates: cache.channels.clone(),
                log: log.to_vec(),
            });
        })
    };
    queue.lock().unwrap().push_back(Event::UpdateCheckFinished {
        updates,
        log,
        checked_at: cache.checked_at,
    });
}

/// 后台下载并校验 app 新版本。
pub(super) fn spawn_app_download(queue: &Queue, manifest: update::AppManifest) {
    spawn_worker(
        queue,
        || Event::BackgroundFailed("软件更新下载".into()),
        move |queue| {
            let data_dir = agent_session_manager::transfer::data_dir();
            let started = std::time::Instant::now();
            let mut last_emit = std::time::Instant::now();
            let progress_queue = queue.clone();
            let result = update::app::download(&manifest, &data_dir, |progress| {
                if last_emit.elapsed().as_millis() >= 200 || progress.downloaded == progress.total {
                    let elapsed = started.elapsed().as_secs_f64().max(0.001);
                    let speed = (progress.downloaded as f64 / elapsed) as u64;
                    progress_queue
                        .lock()
                        .unwrap()
                        .push_back(Event::AppUpdateProgress {
                            downloaded: progress.downloaded,
                            total: progress.total,
                            speed_bps: speed,
                        });
                    last_emit = std::time::Instant::now();
                }
            });
            queue.lock().unwrap().push_back(Event::AppUpdateDownloaded {
                result: result.map_err(|error| format!("{error:#}")),
                version: manifest.version.clone(),
            });
        },
    );
}

/// 后台下载插件 zip，走现有严格导入，并保留原启用状态。
pub(super) fn spawn_plugin_update(queue: &Queue, entry: update::PluginEntry) {
    spawn_worker(
        queue,
        || Event::PluginUpdateFinished {
            id: String::new(),
            version: String::new(),
            result: Err("插件更新任务异常终止".into()),
        },
        move |queue| {
            let data_dir = agent_session_manager::transfer::data_dir();
            let started = std::time::Instant::now();
            let mut last_emit = std::time::Instant::now();
            let progress_queue = queue.clone();
            let result = (|| -> anyhow::Result<String> {
                let zip = update::plugins::download(&entry, &data_dir, |progress| {
                    if last_emit.elapsed().as_millis() >= 200
                        || (progress.total > 0 && progress.downloaded == progress.total)
                    {
                        let elapsed = started.elapsed().as_secs_f64().max(0.001);
                        let speed = (progress.downloaded as f64 / elapsed) as u64;
                        progress_queue
                            .lock()
                            .unwrap()
                            .push_back(Event::PluginUpdateProgress {
                                id: entry.id.clone(),
                                downloaded: progress.downloaded,
                                total: progress.total,
                                speed_bps: speed,
                            });
                        last_emit = std::time::Instant::now();
                    }
                })?;
                progress_queue
                    .lock()
                    .unwrap()
                    .push_back(Event::PluginUpdateDownloaded {
                        id: entry.id.clone(),
                    });
                let was_enabled = plugins::installed()
                    .ok()
                    .and_then(|list| list.into_iter().find(|plugin| plugin.id == entry.id))
                    .map(|plugin| plugin.enabled)
                    .unwrap_or(true);
                let imported = plugins::import_archive(&zip)?;
                let label = imported.info.label.to_string();
                if was_enabled {
                    agents::upsert_external(imported.info)?;
                } else {
                    plugins::set_enabled(&entry.id, false)?;
                }
                let _ = std::fs::remove_file(&zip);
                Ok(label)
            })();
            queue
                .lock()
                .unwrap()
                .push_back(Event::PluginUpdateFinished {
                    id: entry.id.clone(),
                    version: entry.version.clone(),
                    result: result.map_err(|error| format!("{error:#}")),
                });
        },
    );
}

/// 发起一轮刷新；已有任务在跑时只记一个待办，当前任务结束后自动补一轮。
pub(super) fn start_refresh(window: &MainWindow, state: &Rc<RefCell<State>>, queue: &Queue) {
    let Some(generation) = super::state::request_refresh(&mut state.borrow_mut()) else {
        return;
    };
    window.set_loading(true);
    spawn_refresh(queue.clone(), generation);
}

/// 当前代次结束：有待办就立刻开始下一代，否则收起 loading。
pub(super) fn finish_refresh(window: &MainWindow, state: &Rc<RefCell<State>>, queue: &Queue) {
    match super::state::complete_refresh(&mut state.borrow_mut()) {
        Some(generation) => {
            window.set_loading(true);
            spawn_refresh(queue.clone(), generation);
        }
        None => window.set_loading(false),
    }
}

pub(super) fn spawn_refresh(queue: Queue, generation: u32) {
    spawn_worker(
        &queue,
        move || Event::RefreshFailed {
            generation,
            what: "会话刷新".into(),
        },
        move |queue| {
            // 各 agent 存储互不相干，并行读取；单个 agent 失败只记录错误，不影响其余结果。
            let agents = agents::all();
            let mut open_ids = std::collections::HashSet::new();
            for agent in &agents {
                if let Some(ids) = agent.adapter.open_session_ids() {
                    open_ids.extend(ids);
                }
            }
            let open_session_ids = (!open_ids.is_empty()).then_some(open_ids);
            let handles: Vec<_> = agents
                .into_iter()
                .map(|agent| {
                    let id = agent.id.to_string();
                    (
                        id,
                        std::thread::spawn(move || agent.adapter.list_with_warnings()),
                    )
                })
                .collect();
            let mut all = Vec::new();
            let mut errors = Vec::new();
            let mut warnings = Vec::new();
            for (id, handle) in handles {
                match handle.join() {
                    Ok(Ok(listing)) => {
                        all.extend(listing.sessions);
                        warnings.extend(listing.warnings.into_iter().map(|warning| {
                            AgentReadWarning {
                                label: agents::label(&id).to_string(),
                                message: warning.message,
                            }
                        }));
                    }
                    Ok(Err(error)) => errors.push(AgentReadError {
                        label: agents::label(&id).to_string(),
                        message: format!("{error:#}"),
                    }),
                    Err(_) => errors.push(AgentReadError {
                        label: agents::label(&id).to_string(),
                        message: "读取线程异常".into(),
                    }),
                }
            }
            sort_sessions(&mut all);
            queue.lock().unwrap().push_back(Event::Loaded {
                sessions: all,
                open_session_ids,
                errors,
                warnings,
                generation,
            });
        },
    );
}

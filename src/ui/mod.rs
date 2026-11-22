//! 主窗口与事件泵。纯逻辑在 state.rs，渲染在 view.rs，事件类型在 events.rs，
//! 回调绑定在 handlers.rs，后台任务在 tasks.rs，窗口外观在 shell.rs。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use slint::ComponentHandle;

mod assets;
mod events;
mod handlers;
mod pump;
mod shell;
mod single_instance;
mod state;
mod tasks;
mod theme;
mod view;

use agent_session_manager::installs;
use agent_session_manager::time::local_time;
slint::include_modules!();

#[cfg(windows)]
mod chrome;
#[cfg(windows)]
mod dpi;
#[cfg(windows)]
mod native;

use assets::Icons;
use events::{Event, PendingAction, Queue};
use state::State;

fn fmt_time(ms: i64) -> String {
    local_time(ms, "%m-%d %H:%M")
}

/// 窗口回调共享的状态集合。
pub(crate) struct Context {
    pub(crate) window: slint::Weak<MainWindow>,
    pub(crate) state: Rc<RefCell<State>>,
    pub(crate) icons: Rc<Icons>,
    pub(crate) queue: Queue,
    pub(crate) pending: Rc<RefCell<Option<PendingAction>>>,
    pub(crate) dark: bool,
}

pub fn run() -> Result<(), slint::PlatformError> {
    // 已有实例（前台或托盘）时只向它发送显示请求，本进程直接退出。
    let Some(mut instance) = single_instance::claim() else {
        return Ok(());
    };
    // 关闭窗口即隐藏到托盘；销毁窗口可释放约 70MB 的 OpenGL 上下文与表面，托盘恢复时重建。
    std::env::set_var("SLINT_DESTROY_WINDOW_ON_HIDE", "1");
    // 默认构建使用 Skia；--no-default-features 时退回 Slint 自带的 femtovg。
    // 跨屏 DPI 仍由 dpi.rs 的窗口过程钩子处理。
    let renderer = if cfg!(feature = "renderer-skia") {
        "skia"
    } else {
        "femtovg"
    };
    slint::BackendSelector::new()
        .renderer_name(renderer.into())
        .select()?;
    let window = MainWindow::new()?;
    window
        .window()
        .set_size(slint::LogicalSize::new(1080.0, 720.0));
    let tray = Tray::new()?;
    window
        .window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
    window.set_app_version(env!("CARGO_PKG_VERSION").into());
    // 主题调色板来自 assets/themes，壁纸在壁纸主题下按需加载。
    let config = shell::load_config();
    let dark = theme::apply(&window, &config);
    window.set_theme_choice(config.theme.clone().into());
    window.set_normal_tool_results(config.export.normal.tool_results);
    window.set_handoff_tool_results(config.export.handoff.tool_results);
    window.set_compaction_only(config.transfer.compaction_only);
    // 版本检查结果从 updates.json 缓存预热（红点与上次检查跨启动保留）；
    // 检查日志只在内存实时更新，不落盘，启动为空。目录加载警告写入检查日志。
    let update_cache = agent_session_manager::updates::load_cache();
    let mut startup_log = Vec::new();
    for warning in agent_session_manager::plugins::initialize()
        .unwrap_or_else(|error| vec![format!("插件初始化失败：{error:#}")])
    {
        agent_session_manager::updates::push_log(&mut startup_log, warning, false);
    }
    // 插件必须先于图标与目录初始化，界面才能拿到插件自带图标和变体声明。
    let icons = Rc::new(Icons::new());
    let catalog = agent_session_manager::catalog::from_plugins();
    for warning in &catalog.warnings {
        agent_session_manager::updates::push_log(&mut startup_log, warning.clone(), false);
    }
    let state = Rc::new(RefCell::new(State {
        all: Vec::new(),
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
        installs: installs::load_cache(),
        catalog,
        plugins: agent_session_manager::plugins::installed().unwrap_or_default(),
        plugin_importing: false,
        plugin_checking: false,
        plugin_last_check_ms: 0,
        plugin_check_error: String::new(),
        plugin_updates: Vec::new(),
        plugin_index: agent_session_manager::update::plugins::load_index_cache(
            &agent_session_manager::transfer::data_dir(),
        )
        .map(|mut index| {
            // 缓存保存相对地址，启动时按当前索引地址展开。
            let _ = agent_session_manager::update::resolve_plugin_index(
                &agent_session_manager::update::plugin_index_url(),
                &mut index,
            );
            index.plugins
        })
        .unwrap_or_default(),
        plugin_downloads: std::collections::HashMap::new(),
        plugin_installing: std::collections::HashSet::new(),
        plugin_update_errors: std::collections::HashMap::new(),
        plugin_updated: std::collections::HashMap::new(),
        plugin_tab_initialized: std::cell::Cell::new(false),
        plugin_log: Vec::new(),
        sources: agent_session_manager::updates::load_sources(),
        agents_scanning: false,
        groups_collapsed: HashSet::new(),
        garbage: Vec::new(),
        garbage_scanned: false,
        garbage_scanning: false,
        config,
        updates: update_cache.channels.clone(),
        update_log: startup_log,
        checking: false,
        last_check_ms: update_cache.checked_at,
        app_checking: false,
        app_update: None,
        app_update_error: String::new(),
        app_last_check_ms: 0,
        app_download: None,
        app_downloaded: None,
        app_downloaded_version: None,
    }));
    let queue: Queue =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    // 会话列表全程复用同一个 VecModel 实例（apply_view 内 set_vec 原地更新），
    // 替换模型实例会重置 ListView 滚动位置。
    window.set_sessions(Rc::new(slint::VecModel::from(Vec::<SessionRow>::new())).into());
    let pending: Rc<RefCell<Option<PendingAction>>> = Rc::new(RefCell::new(None));
    view::apply_settings(&window, &state.borrow(), &icons);
    view::apply_plugins(&window, &state.borrow(), &icons);

    let ctx = Context {
        window: window.as_weak(),
        state: state.clone(),
        icons,
        queue: queue.clone(),
        pending,
        dark,
    };
    // 定时器必须持有到事件循环结束，否则析构即停。
    let _pump = pump::start(&ctx);
    handlers::bind(&window, &tray, &ctx);
    // 重复启动 exe 时由事件泵恢复窗口；Guard 存活到 run() 结束。
    {
        let queue = queue.clone();
        instance.watch_show(move || {
            queue.lock().unwrap().push_back(Event::ShowWindow);
        });
    }

    // 检查中旋转图标的驱动计时器：仅在 checking 状态推进角度。
    let spinner = slint::Timer::default();
    {
        let weak = ctx.window.clone();
        let state = state.clone();
        spinner.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(80),
            move || {
                {
                    let st = state.borrow();
                    if !st.checking
                        && !st.plugin_checking
                        && !st.app_checking
                        && st.app_download.is_none()
                    {
                        return;
                    }
                }
                if let Some(w) = weak.upgrade() {
                    w.set_spinner_angle((w.get_spinner_angle() + 45.0) % 360.0);
                }
            },
        );
    }

    tasks::start_refresh(&window, &state, &ctx.queue);
    // 自动检查开启时：启动后延迟 5 秒检查一次，之后每 20 分钟重复。
    // 关闭时定时器空转，不发起任何请求。
    if state.borrow().config.check.auto_on_startup {
        {
            let mut st = state.borrow_mut();
            st.checking = true;
            st.app_checking = true;
            st.plugin_checking = true;
        }
        tasks::spawn_auto_checks(&queue, 5, update_cache, Vec::new());
        view::apply_plugins(&window, &state.borrow(), &ctx.icons);
    }
    let auto_check_timer = slint::Timer::default();
    {
        let state = state.clone();
        let queue = queue.clone();
        let icons = ctx.icons.clone();
        let weak = ctx.window.clone();
        auto_check_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(20 * 60),
            move || {
                if !state.borrow().config.check.auto_on_startup {
                    return;
                }
                // 上一次自动或手动检查尚未结束时跳过本次 tick，不排队、不补跑。
                let (spawn, log) = {
                    let mut st = state.borrow_mut();
                    let spawn = !st.checking && !st.app_checking && !st.plugin_checking;
                    if spawn {
                        st.checking = true;
                        st.app_checking = true;
                        st.plugin_checking = true;
                    }
                    (spawn, st.update_log.clone())
                };
                if !spawn {
                    return;
                }
                let cache = agent_session_manager::updates::load_cache();
                tasks::spawn_auto_checks(&queue, 0, cache, log);
                if let Some(w) = weak.upgrade() {
                    view::apply_settings(&w, &state.borrow(), &icons);
                    view::apply_plugins(&w, &state.borrow(), &icons);
                    view::apply_agents(&w, &state.borrow(), &icons);
                }
            },
        );
    }
    #[cfg(windows)]
    shell::apply_title_bar(dark, 10);
    let run_result = window.run();
    // 事件循环返回后先清理插件，再释放单实例句柄；托盘退出仍走 quit_event_loop。
    if let Err(error) = agent_session_manager::plugins::shutdown() {
        eprintln!("插件退出清理失败：{error:#}");
    }
    run_result?;
    Ok(())
}

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use agent_session_manager::agents;
use agent_session_manager::installs;
use agent_session_manager::ir::SessionSummary;
use agent_session_manager::transfer;

use super::MainWindow;

pub type Queue = Arc<Mutex<VecDeque<Event>>>;

pub struct AgentReadError {
    pub label: String,
    pub message: String,
}

pub struct AgentReadWarning {
    pub label: String,
    pub message: String,
}

pub enum Event {
    Loaded {
        sessions: Vec<SessionSummary>,
        open_session_ids: Option<std::collections::HashSet<String>>,
        errors: Vec<AgentReadError>,
        warnings: Vec<AgentReadWarning>,
        generation: u32,
    },
    /// 当前代次的刷新线程异常终止；过期代次的失败直接忽略。
    RefreshFailed {
        generation: u32,
        what: String,
    },
    TransferFinished {
        result: Result<transfer::Outcome, String>,
        target: String,
        target_cwd: String,
        running: bool,
    },
    TargetBlocked {
        target: String,
        request: transfer::Request,
    },
    CloseTransferFinished {
        result: Result<transfer::Outcome, String>,
        target: String,
        reopened: Result<(), String>,
    },
    DeleteFinished {
        result: Result<(), String>,
        count: usize,
        kind: DeleteKind,
    },
    ExportFinished {
        count: usize,
        dir: std::path::PathBuf,
        failed: Vec<String>,
    },
    InstallsScanned(Vec<installs::AgentInstall>),
    /// 壁纸导入完成（成功返回数据目录中的文件名）。
    WallpaperImported(Result<String, String>),
    /// 插件导入完成；Ok 为空串表示用户取消。
    PluginImportFinished(Result<String, String>),
    /// 已选择插件压缩包；None 表示用户取消。
    PluginArchivePicked(Result<Option<std::path::PathBuf>, String>),
    /// 启用、停用或卸载完成；Ok 为空串表示无需提示。
    PluginChanged(Result<String, String>),
    /// 单个渠道检查完成；界面逐条刷新日志与结果。
    UpdateCheckProgress {
        updates: Vec<agent_session_manager::updates::ChannelUpdate>,
        log: Vec<agent_session_manager::updates::LogEntry>,
    },
    UpdateCheckFinished {
        updates: Vec<agent_session_manager::updates::ChannelUpdate>,
        log: Vec<agent_session_manager::updates::LogEntry>,
        checked_at: i64,
    },
    /// App 自更新清单检查完成；error 非空表示本次失败。
    AppUpdateChecked {
        manifest: Option<agent_session_manager::update::AppManifest>,
        error: String,
        checked_at: i64,
    },
    /// App 自更新下载进度。
    AppUpdateProgress {
        downloaded: u64,
        total: u64,
        speed_bps: u64,
    },
    /// App 自更新下载完成；Ok 为下载后的文件路径。
    AppUpdateDownloaded {
        result: Result<std::path::PathBuf, String>,
        version: String,
    },
    /// 插件索引检查完成；plugins 为完整索引（含未安装条目），失败时为 None。
    PluginIndexChecked {
        updates: Vec<agent_session_manager::update::PluginEntry>,
        plugins: Option<Vec<agent_session_manager::update::PluginEntry>>,
        error: String,
        checked_at: i64,
    },
    /// 单个插件更新下载进度。
    PluginUpdateProgress {
        id: String,
        downloaded: u64,
        total: u64,
        speed_bps: u64,
    },
    /// 单个插件 zip 下载完成，开始导入。
    PluginUpdateDownloaded {
        id: String,
    },
    /// 单个插件更新完成；id 为空表示任务异常终止。
    PluginUpdateFinished {
        id: String,
        version: String,
        result: Result<String, String>,
    },
    DestroyFinished {
        report: agent_session_manager::transfer::DestroyReport,
    },
    GarbageScanned {
        groups: Vec<(String, Vec<String>)>,
    },
    OpenFailed(String),
    /// 第二个实例启动：恢复并置顶已有窗口。
    ShowWindow,
    /// 后台线程 panic 时兜底上报，避免 UI 永久停在 loading。
    BackgroundFailed(String),
}

#[derive(Clone, Copy)]
pub enum DeleteKind {
    Selection,
    Garbage,
}

#[derive(Clone)]
pub enum PendingAction {
    Open {
        agent: String,
        cwd: String,
        id: String,
    },
    Restart {
        agent: String,
        cwd: String,
        id: String,
    },
    CloseTransfer(transfer::Request),
    Delete {
        groups: Vec<(String, Vec<String>)>,
        kind: DeleteKind,
    },
}

pub fn show_result(
    w: &MainWindow,
    title: &str,
    message: &str,
    cancel_visible: bool,
    action: Option<PendingAction>,
    pending: &Rc<RefCell<Option<PendingAction>>>,
) {
    *pending.borrow_mut() = action;
    w.set_result_title(title.into());
    w.set_result_message(message.into());
    w.set_result_cancel_visible(cancel_visible);
    w.set_result_open(true);
}

pub fn notice_transferred(
    w: &MainWindow,
    target: &str,
    target_cwd: &str,
    target_id: &str,
    running: bool,
    title: &str,
    prefix: &str,
    pending: &Rc<RefCell<Option<PendingAction>>>,
) {
    let label = agents::label(target);
    if running {
        show_result(
            w,
            title,
            &format!(
                "{prefix}{label} 正在运行，需要重新启动后才能加载该会话。是否重新启动 {label}？"
            ),
            true,
            Some(PendingAction::Restart {
                agent: target.to_string(),
                cwd: target_cwd.to_string(),
                id: target_id.to_string(),
            }),
            pending,
        );
    } else {
        show_result(
            w,
            title,
            &format!("{prefix}是否现在打开 {label} 加载会话？"),
            true,
            Some(PendingAction::Open {
                agent: target.to_string(),
                cwd: target_cwd.to_string(),
                id: target_id.to_string(),
            }),
            pending,
        );
    }
}

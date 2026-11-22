#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

fn main() -> Result<(), slint::PlatformError> {
    install_panic_hook();
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("doctor" | "transfer" | "plugin")) {
        attach_console();
    }
    match arg.as_deref() {
        Some("doctor") => {
            let _ = agent_session_manager::plugins::initialize();
            doctor();
            if let Err(error) = agent_session_manager::plugins::shutdown() {
                eprintln!("插件退出清理失败：{error:#}");
            }
        }
        Some("transfer") => {
            let _ = agent_session_manager::plugins::initialize();
            let code = transfer_cmd();
            if let Err(error) = agent_session_manager::plugins::shutdown() {
                eprintln!("插件退出清理失败：{error:#}");
                std::process::exit(1);
            }
            if code != 0 {
                std::process::exit(code);
            }
        }
        Some("plugin") => {
            if let Err(error) = plugin_cmd() {
                eprintln!("{error:#}");
                let _ = agent_session_manager::plugins::shutdown();
                std::process::exit(1);
            }
            if let Err(error) = agent_session_manager::plugins::shutdown() {
                eprintln!("插件退出清理失败：{error:#}");
                std::process::exit(1);
            }
        }
        _ => return ui::run(),
    }
    Ok(())
}

// 安全加固（见 docs/security.md）：默认 panic 输出携带 src\xxx.rs:行号 的
// 位置信息，会把模块拓扑带进控制台与日志。release 下只输出消息本身；debug 保留默认
// 行为便于开发调试。调试需要完整位置时临时摘掉本 hook 或用 debug 构建。
#[cfg(not(debug_assertions))]
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "无描述".to_string());
        eprintln!("应用遇到内部错误：{msg}");
    }));
}

#[cfg(debug_assertions)]
fn install_panic_hook() {}

#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // ATTACH_PARENT_PROCESS = (DWORD)-1；无父控制台时附加失败，忽略返回值并静默丢弃输出。
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    // SAFETY: 仅调用 Win32 AttachConsole 附加父控制台，不改进程其他状态。
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}

fn transfer_cmd() -> i32 {
    use agent_session_manager::agents;
    use agent_session_manager::transfer::{self, Mode, Outcome, Request};
    let args: Vec<String> = std::env::args().collect();
    let usage = "用法: agent-session-manager transfer <agent> <session-id> [--to <agent>] [--export] [--cwd <path>]";
    let (Some(src), Some(id)) = (args.get(2), args.get(3)) else {
        eprintln!("{usage}");
        return 2;
    };
    let source = agents::normalize(src);
    if agents::get(&source).is_none() {
        eprintln!("未知 agent：{src}\n{usage}");
        return 2;
    }
    let mut export = false;
    let mut cwd: Option<String> = None;
    let mut target: Option<String> = None;
    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--export" => export = true,
            "--to" => {
                i += 1;
                target = args.get(i).map(|s| agents::normalize(s));
            }
            "--cwd" => {
                i += 1;
                cwd = args.get(i).cloned();
            }
            other => {
                eprintln!("未知参数 {other}\n{usage}");
                return 2;
            }
        }
        i += 1;
    }
    let target = target.unwrap_or_else(|| {
        agents::all()
            .iter()
            .find(|a| a.can_write && a.id.as_ref() != source)
            .map(|a| a.id.to_string())
            .unwrap_or_default()
    });
    // 不传 --cwd 时留空，由写入器使用 execute() 已加载会话的工作区，避免为取 cwd 再扫一遍列表。
    let target_cwd = cwd.unwrap_or_default();
    let req = Request {
        source,
        target,
        source_id: id.clone(),
        target_cwd,
        mode: if export { Mode::Export } else { Mode::Native },
    };
    match transfer::execute(&req) {
        Ok(Outcome::Injected { target_id }) => println!("injected {target_id}"),
        Ok(Outcome::Updated { target_id }) => println!("updated {target_id}"),
        Ok(Outcome::Exported { path }) => println!("exported {}", path.display()),
        Ok(Outcome::Already { target_id }) => println!("already {target_id}"),
        Err(e) => {
            eprintln!("failed: {e:#}");
            return 1;
        }
    }
    0
}

fn plugin_cmd() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let usage = "用法: agent-session-manager plugin <import <archive>|list>";
    match args.get(2).map(String::as_str) {
        Some("import") => {
            let Some(source) = args.get(3) else {
                anyhow::bail!("{usage}");
            };
            let source = std::path::PathBuf::from(source);
            eprintln!("插件将以当前用户权限运行；请只导入可信来源的压缩包。");
            let plugin = agent_session_manager::plugins::import_archive(&source)?;
            let id = plugin.info.id.to_string();
            let label = plugin.info.label.to_string();
            println!("imported {id} ({label})");
            Ok(())
        }
        Some("list") => {
            agent_session_manager::plugins::initialize()?;
            let manager = agent_session_manager::plugins::manager()?;
            let registry = manager.registry().load()?;
            println!("generation={}", registry.generation);
            for (id, plugin) in &registry.plugins {
                println!(
                    "{id} {} {} enabled={} root={}",
                    plugin.registration.version,
                    plugin.registration.sha256,
                    plugin.enabled,
                    plugin.root
                );
            }
            Ok(())
        }
        _ => anyhow::bail!("{usage}"),
    }
}

fn doctor() {
    use agent_session_manager::{agents, transfer};
    for agent in agents::all() {
        println!(
            "{} ({})  process={}  launch={}  can_write={}",
            agent.label, agent.id, agent.process, agent.launch, agent.can_write
        );
        for (label, path) in agent.adapter.diagnostics() {
            println!("  {label:<10} {}  存在={}", path.display(), path.exists());
        }
        match agent.adapter.list() {
            Ok(sessions) => println!("  会话读取   {} 条", sessions.len()),
            Err(error) => println!("  会话读取   失败：{error:#}"),
        }
        println!("  运行中     {}", agents::running(&agent));
    }
    if let Ok(manager) = agent_session_manager::plugins::manager() {
        let registry = manager.registry().load();
        println!(
            "插件            registry_generation={:?} active_processes={}",
            registry.as_ref().map(|registry| registry.generation),
            manager.active_count()
        );
    }
    println!("数据目录        {}", transfer::data_dir().display());
}

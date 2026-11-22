use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

use super::manifest::PluginManifest;
use super::registry::RegistryWriter;
use agent_core::protocol::{
    PluginRegistration, RpcRequest, RpcResponse, MAX_MESSAGE_BYTES, PLUGIN_TIMEOUT, REGISTER_ID,
    REQUEST_ID,
};

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    root: PathBuf,
    registry: RegistryWriter,
    active: Mutex<HashMap<u32, Arc<Mutex<Child>>>>,
    shutting_down: AtomicBool,
    lifecycle: RwLock<()>,
    job: JobGuard,
}

impl PluginManager {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        Ok(Self {
            inner: Arc::new(ManagerInner {
                registry: RegistryWriter::new(&root),
                root,
                active: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
                lifecycle: RwLock::new(()),
                job: JobGuard::new()?,
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn registry(&self) -> &RegistryWriter {
        &self.inner.registry
    }

    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::SeqCst)
    }

    pub fn active_count(&self) -> usize {
        self.inner
            .active
            .lock()
            .map(|active| active.len())
            .unwrap_or_default()
    }

    pub fn request(
        &self,
        root: &Path,
        manifest: &PluginManifest,
        envs: &[(String, String)],
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.request_with(root, manifest, envs, method, params, true)
    }

    /// 导入校验用探测：同样完成注册握手，但不写入 registry。只有探测成功后
    /// 导入流程才提交注册，避免坏包留下无效登记。
    pub fn probe(&self, root: &Path, manifest: &PluginManifest, method: &str) -> Result<Value> {
        self.request_with(
            root,
            manifest,
            &[],
            method,
            Value::Object(Default::default()),
            false,
        )
    }

    fn request_with(
        &self,
        root: &Path,
        manifest: &PluginManifest,
        envs: &[(String, String)],
        method: &str,
        params: Value,
        persist_registration: bool,
    ) -> Result<Value> {
        // 与 shutdown 的写锁互斥：要么本次子进程已经进入活动表，要么关闭后
        // 请求看到 shutting_down 并拒绝，不能留下关闭时看不见的插件进程。
        let lifecycle = self
            .inner
            .lifecycle
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_shutting_down() {
            anyhow::bail!("应用正在关闭，已拒绝新的插件请求");
        }
        let command = manifest.command_path(root)?;
        let mut command = Command::new(command);
        command
            .args(&manifest.execution.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);
        for (name, value) in envs {
            command.env(name, value);
        }
        let mut child = command.spawn().context("启动短命插件进程失败")?;
        self.inner.job.assign(&child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        let pid = child.id();
        let stdout = child.stdout.take().context("无法接管插件 stdout")?;
        let child = Arc::new(Mutex::new(child));
        self.inner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(pid, child.clone());
        drop(lifecycle);

        let deadline = Instant::now() + Duration::from_millis(manifest.execution.timeout_ms);
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || read_protocol_lines(stdout, sender));
        let result = self.exchange(
            root,
            manifest,
            child.clone(),
            &receiver,
            deadline,
            method,
            params,
            persist_registration,
        );
        self.finish_child(pid, child);
        let _ = reader.join();
        result
    }

    fn exchange(
        &self,
        root: &Path,
        manifest: &PluginManifest,
        child: Arc<Mutex<Child>>,
        receiver: &Receiver<Result<String>>,
        deadline: Instant,
        method: &str,
        params: Value,
        persist_registration: bool,
    ) -> Result<Value> {
        let registration = receive_line(receiver, deadline).context("读取插件注册申请失败")?;
        let request: RpcRequest =
            serde_json::from_str(&registration).context("插件注册申请不是有效 JSON-RPC 请求")?;
        if request.id != REGISTER_ID || request.method != "plugin.register" {
            anyhow::bail!("插件必须先发送 plugin.register 注册申请");
        }
        let registration: PluginRegistration =
            serde_json::from_value(request.params).context("插件注册申请参数无效")?;
        let expected = manifest.registration();
        if registration != expected {
            anyhow::bail!(
                "插件注册申请与已安装包不一致：期望 {} {}，收到 {} {}",
                expected.id,
                expected.version,
                registration.id,
                registration.version
            );
        }
        let relative_root = relative_package_root(&self.inner.root, root)?;
        let response = if persist_registration {
            match self.inner.registry.register(&registration, &relative_root) {
                Ok(generation) => RpcResponse::success(
                    REGISTER_ID,
                    serde_json::json!({ "accepted": true, "generation": generation }),
                ),
                Err(error) => {
                    let response = RpcResponse::failure(
                        REGISTER_ID,
                        agent_core::protocol::PLUGIN_CONFLICT,
                        "plugin_conflict",
                        format!("{error:#}"),
                    );
                    write_child_line(&child, &response)?;
                    return Err(error.context("插件注册申请被拒绝"));
                }
            }
        } else {
            RpcResponse::success(
                REGISTER_ID,
                serde_json::json!({ "accepted": true, "probe": true }),
            )
        };
        write_child_line(&child, &response)?;

        let request = RpcRequest::new(REQUEST_ID, method, params);
        write_child_line(&child, &request)?;
        let response = receive_line(receiver, deadline).context("读取插件响应失败")?;
        let response: RpcResponse =
            serde_json::from_str(&response).context("插件响应不是有效 JSON-RPC")?;
        response.validate(REQUEST_ID)?;
        response.into_result()
    }

    fn finish_child(&self, pid: u32, child: Arc<Mutex<Child>>) {
        self.inner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&pid);
        let start = Instant::now();
        let mut lock = child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = lock.stdin.take();
        while start.elapsed() < SHUTDOWN_TIMEOUT {
            match lock.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => return,
            }
        }
        terminate_process_tree(pid);
        if let Err(error) = lock.kill() {
            if lock.try_wait().ok().flatten().is_none() {
                eprintln!("终止插件进程 {pid} 失败：{error}");
            }
        }
        let _ = lock.wait();
    }

    /// 宿主退出前的统一清理：拒绝新请求，先关闭 stdin 等待，再整树终止。
    /// 该函数只管理本次进程启动的子进程，不会按映像名结束用户自己的 agent。
    pub fn shutdown(&self) -> Result<()> {
        let lifecycle = self
            .inner
            .lifecycle
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let children: Vec<(u32, Arc<Mutex<Child>>)> = self
            .inner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(pid, child)| (*pid, child.clone()))
            .collect();
        drop(lifecycle);
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        for (_, child) in &children {
            if let Ok(mut lock) = child.lock() {
                let _ = lock.stdin.take();
            }
        }
        let mut errors = Vec::new();
        for (pid, child) in children {
            let mut lock = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                match lock.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        drop(lock);
                        thread::sleep(Duration::from_millis(10));
                        lock = child
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                    Ok(None) => {
                        terminate_process_tree(pid);
                        if let Err(error) = lock.kill() {
                            errors.push(format!("终止插件进程 {pid} 失败：{error}"));
                        }
                        let _ = lock.wait();
                        break;
                    }
                    Err(error) => {
                        errors.push(format!("查询插件进程 {pid} 状态失败：{error}"));
                        break;
                    }
                }
            }
            self.inner
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&pid);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("；")))
        }
    }
}

impl Drop for ManagerInner {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        let children: Vec<(u32, Arc<Mutex<Child>>)> = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .collect();
        for (pid, child) in children {
            let mut lock = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = lock.stdin.take();
            if lock.try_wait().ok().flatten().is_none() {
                terminate_process_tree(pid);
                let _ = lock.kill();
                let _ = lock.wait();
            }
        }
    }
}

fn read_protocol_lines(stdout: ChildStdout, sender: mpsc::Sender<Result<String>>) {
    let mut reader = BufReader::new(stdout);
    for _ in 0..2 {
        let mut line = String::new();
        let result = reader
            .read_line(&mut line)
            .map_err(anyhow::Error::from)
            .and_then(|read| {
                if read == 0 {
                    anyhow::bail!("插件提前关闭了 stdout")
                } else if line.len() > MAX_MESSAGE_BYTES {
                    anyhow::bail!("JSON-RPC 消息超过 {MAX_MESSAGE_BYTES} 字节")
                } else {
                    Ok(line.clone())
                }
            });
        if sender.send(result).is_err() {
            return;
        }
    }
}

fn receive_line(receiver: &Receiver<Result<String>>, deadline: Instant) -> Result<String> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| anyhow::anyhow!("插件请求超时（错误码 {PLUGIN_TIMEOUT}）"))?;
    match receiver.recv_timeout(remaining) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("插件请求超时（错误码 {PLUGIN_TIMEOUT}）")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("插件输出通道意外关闭"),
    }
}

fn write_child_line(child: &Arc<Mutex<Child>>, value: &impl serde::Serialize) -> Result<()> {
    let mut lock = child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stdin = lock.stdin.as_mut().context("插件 stdin 已关闭")?;
    serde_json::to_writer(&mut *stdin, value)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn relative_package_root(manager_root: &Path, package_root: &Path) -> Result<PathBuf> {
    let package_root = package_root.canonicalize()?;
    Ok(package_root.strip_prefix(manager_root)?.to_path_buf())
}

fn terminate_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = command.status();
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

#[cfg(windows)]
struct JobGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: HANDLE 是进程级有效的不透明指针，没有借用指向的数据；
// JobGuard 通过 Drop 拥有该句柄，跨线程转移或共享时同一时刻仍只有一个所有者。
#[cfg(windows)]
unsafe impl Send for JobGuard {}
#[cfg(windows)]
unsafe impl Sync for JobGuard {}

#[cfg(windows)]
impl JobGuard {
    fn new() -> Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("创建插件 Job Object 失败");
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let result = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if result == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(error).context("配置插件 Job Object 失败");
            }
            Ok(Self { handle })
        }
    }

    fn assign(&self, child: &Child) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        let result = unsafe {
            AssignProcessToJobObject(
                self.handle,
                child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
            )
        };
        if result == 0 {
            Err(std::io::Error::last_os_error()).context("将插件进程加入 Job Object 失败")
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
struct JobGuard;

#[cfg(not(windows))]
impl JobGuard {
    fn new() -> Result<Self> {
        Ok(Self)
    }

    fn assign(&self, _child: &Child) -> Result<()> {
        Ok(())
    }
}

//! 单实例守卫：命名互斥量判定首个实例，命名事件把「再次启动 exe」转成唤醒
//! 已有窗口的请求。第二个实例发完请求即退出，不再创建窗口与托盘。

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    const ERROR_ALREADY_EXISTS: u32 = 183;
    const WAIT_OBJECT_0: u32 = 0;
    const INFINITE: u32 = u32::MAX;

    const MUTEX_NAME: &str = "Local\\AgentSessionManager.SingleInstance";
    const SHOW_EVENT_NAME: &str = "Local\\AgentSessionManager.ShowRequest";

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *mut c_void, owner: i32, name: *const u16) -> *mut c_void;
        fn CreateEventW(
            attrs: *mut c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn SetEvent(event: *mut c_void) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
        fn SetLastError(code: u32);
    }

    /// 首个实例持有 Guard 直到事件循环结束；互斥量随句柄关闭释放。
    pub(crate) struct Guard {
        mutex: *mut c_void,
        show_event: Option<*mut c_void>,
    }

    impl Guard {
        /// 在后台线程等待唤醒事件；`on_show` 不得直接操作窗口。
        pub(crate) fn watch_show<F>(&mut self, on_show: F)
        where
            F: Fn() + Send + 'static,
        {
            let Some(event) = self.show_event.take() else {
                return;
            };
            // 句柄交给等待线程持有，Guard 析构时不再关闭它。
            let handle = event as usize;
            std::thread::spawn(move || {
                let event = handle as *mut c_void;
                loop {
                    let rc = unsafe { WaitForSingleObject(event, INFINITE) };
                    if rc != WAIT_OBJECT_0 {
                        unsafe { CloseHandle(event) };
                        return;
                    }
                    on_show();
                }
            });
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                if let Some(event) = self.show_event.take() {
                    CloseHandle(event);
                }
                if !self.mutex.is_null() {
                    CloseHandle(self.mutex);
                }
            }
        }
    }

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 首个实例返回 Some；已有实例时向它发送显示请求并返回 None。
    pub(crate) fn claim() -> Option<Guard> {
        claim_named(MUTEX_NAME, SHOW_EVENT_NAME)
    }

    fn claim_named(mutex_name: &str, event_name: &str) -> Option<Guard> {
        unsafe {
            SetLastError(0);
            let mutex = CreateMutexW(std::ptr::null_mut(), 0, wide(mutex_name).as_ptr());
            if mutex.is_null() {
                // 守卫创建失败时照常启动，不因系统调用异常挡住界面。
                return Some(Guard {
                    mutex,
                    show_event: None,
                });
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(mutex);
                // CreateEventW 在事件尚未建立时创建，已建立时打开同名对象，
                // 覆盖首个实例尚未开始等待的启动窗口。
                let event = CreateEventW(std::ptr::null_mut(), 0, 0, wide(event_name).as_ptr());
                if !event.is_null() {
                    SetEvent(event);
                    CloseHandle(event);
                }
                return None;
            }
            let show_event = CreateEventW(std::ptr::null_mut(), 0, 0, wide(event_name).as_ptr());
            Some(Guard {
                mutex,
                show_event: (!show_event.is_null()).then_some(show_event),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::Duration;

        fn names(tag: &str) -> (String, String) {
            let tag = format!("{}.{}", tag, uuid::Uuid::new_v4());
            (
                format!("Local\\AgentSessionManager.Test.Mutex.{tag}"),
                format!("Local\\AgentSessionManager.Test.Event.{tag}"),
            )
        }

        #[test]
        fn second_claim_reports_existing_instance() {
            let (mutex, event) = names("claim");
            let first = claim_named(&mutex, &event).expect("首个实例应拿到守卫");
            assert!(claim_named(&mutex, &event).is_none());
            drop(first);
            assert!(claim_named(&mutex, &event).is_some());
        }

        #[test]
        fn second_claim_signals_show_request() {
            let (mutex, event) = names("show");
            let mut first = claim_named(&mutex, &event).expect("首个实例应拿到守卫");
            let (tx, rx) = std::sync::mpsc::channel();
            first.watch_show(move || {
                let _ = tx.send(());
            });
            assert!(claim_named(&mutex, &event).is_none());
            rx.recv_timeout(Duration::from_secs(2))
                .expect("显示请求应送达等待线程");
            // 自动重置事件：下一次重复启动仍能唤醒。
            assert!(claim_named(&mutex, &event).is_none());
            rx.recv_timeout(Duration::from_secs(2))
                .expect("第二次显示请求应送达");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub(crate) struct Guard;

    impl Guard {
        pub(crate) fn watch_show<F>(&mut self, _on_show: F)
        where
            F: Fn() + Send + 'static,
        {
        }
    }

    pub(crate) fn claim() -> Option<Guard> {
        Some(Guard)
    }
}

pub(crate) use imp::claim;

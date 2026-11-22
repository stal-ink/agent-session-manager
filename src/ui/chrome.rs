//! Windows: 自绘标题栏的窗口命中测试。
//!
//! 标题栏空白区不是 Slint 的交互元素，`WindowMoveArea` 只覆盖它的内容宽度。
//! 这里在窗口过程里把标题栏报成 `HTCAPTION`，拖动、双击最大化和系统菜单交给
//! Windows 处理；右侧窗口按钮与缩放边缘仍返回客户端，交给 Slint 和 winit。

#[cfg(all(windows, target_pointer_width = "64"))]
mod imp {
    use std::cell::RefCell;

    type Hwnd = isize;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        fn CallWindowProcW(prev: isize, hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
        fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn ScreenToClient(hwnd: Hwnd, point: *mut Point) -> i32;
        fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    }

    const GWLP_WNDPROC: i32 = -4;
    const WM_NCDESTROY: u32 = 0x0082;
    const WM_NCHITTEST: u32 = 0x0084;
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const HTCAPTION: isize = 2;
    const TITLE_BAR_LOGICAL: f64 = 44.0;
    const TITLE_CONTROLS_LOGICAL: f64 = 126.0;
    const RESIZE_BORDER_LOGICAL: f64 = 6.0;

    #[derive(Clone, Copy)]
    struct Hook {
        hwnd: Hwnd,
        prev: isize,
    }

    thread_local! {
        static HOOKS: RefCell<Vec<Hook>> = const { RefCell::new(Vec::new()) };
        static MOUSE_DOWN: RefCell<Option<Box<dyn Fn(i32, i32, i32)>>> = const { RefCell::new(None) };
    }

    fn find(hwnd: Hwnd) -> Option<Hook> {
        HOOKS.with_borrow(|hooks| hooks.iter().copied().find(|hook| hook.hwnd == hwnd))
    }

    pub fn install(hwnd: Hwnd, on_mouse_down: impl Fn(i32, i32, i32) + 'static) -> bool {
        MOUSE_DOWN.with_borrow_mut(|slot| *slot = Some(Box::new(on_mouse_down)));
        if find(hwnd).is_some() {
            return false;
        }
        let proc_addr = window_proc as unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;
        // SAFETY: `hwnd` is a live window owned by this thread; the previous
        // procedure is kept and every other message is forwarded to it.
        let prev = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc_addr as usize as isize) };
        if prev == 0 {
            return false;
        }
        HOOKS.with_borrow_mut(|hooks| hooks.push(Hook { hwnd, prev }));
        true
    }

    fn client_point(hwnd: Hwnd, lparam: isize) -> Option<(i32, i32, i32)> {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            return None;
        }
        let scale = f64::from(dpi) / 96.0;
        let bits = lparam as u32;
        let point = Point {
            x: (bits & 0xFFFF) as i16 as i32,
            y: ((bits >> 16) & 0xFFFF) as i16 as i32,
        };
        let mut rect = Rect::default();
        if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
            return None;
        }
        let width = ((rect.right - rect.left) as f64 / scale).round() as i32;
        Some((
            (f64::from(point.x) / scale).round() as i32,
            (f64::from(point.y) / scale).round() as i32,
            width,
        ))
    }

    fn is_title_bar(hwnd: Hwnd, lparam: isize) -> bool {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            return false;
        }
        let scale = f64::from(dpi) / 96.0;
        let bits = lparam as u32;
        let mut point = Point {
            x: (bits & 0xFFFF) as i16 as i32,
            y: ((bits >> 16) & 0xFFFF) as i16 as i32,
        };
        if unsafe { ScreenToClient(hwnd, &mut point) } == 0 {
            return false;
        }
        let mut rect = Rect::default();
        if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
            return false;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let edge = (RESIZE_BORDER_LOGICAL * scale).round() as i32;
        let title = (TITLE_BAR_LOGICAL * scale).round() as i32;
        let controls = (TITLE_CONTROLS_LOGICAL * scale).round() as i32;
        point.x >= edge
            && point.x < width - edge
            && point.x < width - controls
            && point.y >= edge
            && point.y < title
            && point.y < height
    }

    unsafe extern "system" fn window_proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        let Some(hook) = find(hwnd) else {
            // SAFETY: default handling for a window this module no longer tracks.
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        };
        // SAFETY: forwards to the procedure that was installed before ours.
        let forward = || unsafe { CallWindowProcW(hook.prev, hwnd, msg, wparam, lparam) };
        match msg {
            WM_NCHITTEST => {
                if is_title_bar(hwnd, lparam) {
                    return HTCAPTION;
                }
            }
            WM_LBUTTONDOWN => {
                if let Some((x, y, width)) = client_point(hwnd, lparam) {
                    MOUSE_DOWN.with_borrow(|slot| {
                        if let Some(callback) = slot.as_ref() {
                            callback(x, y, width);
                        }
                    });
                }
            }
            WM_NCDESTROY => {
                // SAFETY: restores the original procedure before the window goes away.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, hook.prev) };
                HOOKS.with_borrow_mut(|hooks| hooks.retain(|hook| hook.hwnd != hwnd));
                return forward();
            }
            _ => {}
        }
        forward()
    }
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
mod imp {
    pub fn install(_hwnd: isize, _on_mouse_down: impl Fn(i32, i32, i32) + 'static) -> bool {
        false
    }
}

/// 窗口创建后安装一次；窗口隐藏后重建时会再次调用。
/// `on_mouse_down` 收到逻辑坐标与窗口逻辑宽度，用于搜索框失焦判定。
pub(super) fn ensure(window: &slint::Window, on_mouse_down: impl Fn(i32, i32, i32) + 'static) {
    if let Some(hwnd) = super::native::hwnd(window) {
        imp::install(hwnd, on_mouse_down);
    }
}

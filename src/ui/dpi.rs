//! Windows：跨不同缩放比例的显示器拖动窗口时保持窗口尺寸稳定。
//!
//! winit 对 WM_DPICHANGED 的应答使用它自己计算的矩形，而不是 Windows 建议的
//! 矩形。标题栏拖动过程中该矩形可能横跨显示器边界，DPI 因此立即回翻；系统
//! 移动循环随后再缩放一次，在 150%/100% 的组合下每次翻转约放大 1.5 倍。窗口
//! 过程被包装后：
//! - WM_GETDPISCALEDSIZE 返回 winit 将使用的尺寸（客户区保持逻辑尺寸），
//!   使建议值与 winit 一致；
//! - winit 在 WM_DPICHANGED 内部发起的 resize 精确落在建议矩形上，
//!   Windows 会把窗口保留在新显示器；
//! - WM_GETMINMAXINFO 使用当前 DPI 下的逻辑最小值；交给 winit 的最小值是
//!   上一台显示器的物理尺寸；
//! - 移动或缩放结束时，把大于显示器工作区的窗口收缩到工作区内；钩子安装
//!   时也执行一次。
//!
//! 本应用窗口无边框：GetClientRect 与 GetWindowRect 报告相同尺寸。
//! AdjustWindowRectExForDpi 仍按窗口样式补算边框，即使窗口没有非客户区，
//! 于是在每次跨越 96/144 DPI 时都会引入幻影边框。钩子识别无边框情形并直接
//! 缩放客户矩形；只有带真实非客户区的窗口才做边框补偿。

#[cfg(all(windows, target_pointer_width = "64"))]
mod imp {
    use std::cell::{Cell, RefCell};

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
    struct SizeOut {
        cx: i32,
        cy: i32,
    }

    #[repr(C)]
    struct WindowPos {
        hwnd: isize,
        insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct MinMaxInfo {
        reserved: Point,
        max_size: Point,
        max_position: Point,
        min_track: Point,
        max_track: Point,
    }

    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        fn CallWindowProcW(prev: isize, hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
        fn DefWindowProcW(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize;
        fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32;
        fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetDpiForWindow(hwnd: Hwnd) -> u32;
        fn AdjustWindowRectExForDpi(r: *mut Rect, style: u32, menu: i32, ex: u32, dpi: u32) -> i32;
        fn IsZoomed(hwnd: Hwnd) -> i32;
        fn IsIconic(hwnd: Hwnd) -> i32;
        fn MonitorFromWindow(hwnd: Hwnd, flags: u32) -> isize;
        fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
        fn SetWindowPos(hwnd: Hwnd, after: isize, x: i32, y: i32, cx: i32, cy: i32, f: u32) -> i32;
    }

    const GWLP_WNDPROC: i32 = -4;
    const GWL_STYLE: i32 = -16;
    const GWL_EXSTYLE: i32 = -20;
    const WM_GETMINMAXINFO: u32 = 0x0024;
    const WM_WINDOWPOSCHANGING: u32 = 0x0046;
    const WM_NCDESTROY: u32 = 0x0082;
    const WM_EXITSIZEMOVE: u32 = 0x0232;
    const WM_DPICHANGED: u32 = 0x02E0;
    const WM_GETDPISCALEDSIZE: u32 = 0x02E4;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const MONITOR_DEFAULTTONEAREST: u32 = 2;

    #[derive(Clone, Copy)]
    struct Hook {
        hwnd: Hwnd,
        prev: isize,
        /// Logical minimum client size.
        min: (f64, f64),
    }

    thread_local! {
        static HOOKS: RefCell<Vec<Hook>> = const { RefCell::new(Vec::new()) };
        /// Suggested rectangle while WM_DPICHANGED is being handled.
        static SUGGESTED: Cell<Option<Rect>> = const { Cell::new(None) };
    }

    fn find(hwnd: Hwnd) -> Option<Hook> {
        HOOKS.with_borrow(|hooks| hooks.iter().copied().find(|hook| hook.hwnd == hwnd))
    }

    /// Wraps the window procedure once per window. Returns false when the
    /// window was already wrapped or the call failed.
    pub fn install(hwnd: Hwnd, min: (f32, f32)) -> bool {
        if find(hwnd).is_some() {
            return false;
        }
        let proc_addr = window_proc as unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;
        // SAFETY: `hwnd` is a live window owned by this thread; the previous
        // procedure is kept and every message is forwarded to it.
        let prev = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc_addr as usize as isize) };
        if prev == 0 {
            return false;
        }
        let min = (f64::from(min.0), f64::from(min.1));
        HOOKS.with_borrow_mut(|hooks| hooks.push(Hook { hwnd, prev, min }));
        // SAFETY: plain Win32 calls on the live window.
        unsafe { fit_to_work_area(hwnd) };
        true
    }

    /// Same arithmetic as winit's WM_DPICHANGED handler, so the suggested
    /// size and the size winit applies agree to the pixel.
    fn rescale(value: i32, old_dpi: u32, new_dpi: u32) -> i32 {
        (f64::from(value) / (f64::from(old_dpi) / 96.0) * (f64::from(new_dpi) / 96.0)).round()
            as i32
    }

    /// True when GetWindowRect and GetClientRect describe the same area.
    /// The borderless window satisfies this even though its style still makes
    /// AdjustWindowRectExForDpi report a non-client frame.
    fn is_frameless(window: Rect, client: Rect) -> bool {
        window.right - window.left == client.right && window.bottom - window.top == client.bottom
    }

    /// Physical client size for `new_dpi`: no frame is added here, so a
    /// borderless window keeps exactly the logical size the user sees.
    fn scaled_client_size(client: Rect, old_dpi: u32, new_dpi: u32) -> SizeOut {
        SizeOut {
            cx: rescale(client.right, old_dpi, new_dpi),
            cy: rescale(client.bottom, old_dpi, new_dpi),
        }
    }

    unsafe fn with_frame(hwnd: Hwnd, mut rect: Rect, dpi: u32) -> Option<Rect> {
        // SAFETY: reads styles of a live window and adjusts a local rect.
        unsafe {
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            (AdjustWindowRectExForDpi(&mut rect, style, 0, ex, dpi) != 0).then_some(rect)
        }
    }

    /// Outer size for `new_dpi` that keeps the client area's logical size.
    unsafe fn scaled_size(hwnd: Hwnd, new_dpi: u32) -> Option<SizeOut> {
        // SAFETY: queries on a live window; `client` and `window` are locals.
        unsafe {
            let old_dpi = GetDpiForWindow(hwnd);
            if old_dpi == 0 || new_dpi == 0 || IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 {
                return None;
            }
            let mut client = Rect::default();
            if GetClientRect(hwnd, &mut client) == 0 {
                return None;
            }
            let mut window = Rect::default();
            if GetWindowRect(hwnd, &mut window) == 0 {
                return None;
            }
            let scaled = scaled_client_size(client, old_dpi, new_dpi);
            // The borderless window's client already covers the whole window.
            // AdjustWindowRectExForDpi would add the frame implied by its style
            // and grow the window by that phantom frame on every crossing.
            if is_frameless(window, client) {
                return Some(scaled);
            }
            let inner = Rect {
                left: 0,
                top: 0,
                right: scaled.cx,
                bottom: scaled.cy,
            };
            let outer = with_frame(hwnd, inner, new_dpi)?;
            Some(SizeOut {
                cx: outer.right - outer.left,
                cy: outer.bottom - outer.top,
            })
        }
    }

    unsafe fn min_track_size(hwnd: Hwnd, (width, height): (f64, f64)) -> Option<Point> {
        // SAFETY: queries on a live window.
        unsafe {
            let dpi = GetDpiForWindow(hwnd);
            if dpi == 0 || width <= 0.0 || height <= 0.0 {
                return None;
            }
            let scale = f64::from(dpi) / 96.0;
            let inner = Rect {
                left: 0,
                top: 0,
                right: (width * scale).ceil() as i32,
                bottom: (height * scale).ceil() as i32,
            };
            let mut client = Rect::default();
            let mut window = Rect::default();
            if GetClientRect(hwnd, &mut client) != 0
                && GetWindowRect(hwnd, &mut window) != 0
                && is_frameless(window, client)
            {
                return Some(Point {
                    x: inner.right,
                    y: inner.bottom,
                });
            }
            let outer = with_frame(hwnd, inner, dpi)?;
            Some(Point {
                x: outer.right - outer.left,
                y: outer.bottom - outer.top,
            })
        }
    }

    /// Shrinks a window that is larger than its monitor's work area and
    /// moves it fully onto that monitor.
    unsafe fn fit_to_work_area(hwnd: Hwnd) {
        // SAFETY: queries and one SetWindowPos on a live window; all
        // structures are locals with their size fields set.
        unsafe {
            if IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 {
                return;
            }
            let mut window = Rect::default();
            if GetWindowRect(hwnd, &mut window) == 0 {
                return;
            }
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MonitorInfo {
                size: std::mem::size_of::<MonitorInfo>() as u32,
                monitor: Rect::default(),
                work: Rect::default(),
                flags: 0,
            };
            if monitor == 0 || GetMonitorInfoW(monitor, &mut info) == 0 {
                return;
            }
            let work = info.work;
            let (width, height) = (window.right - window.left, window.bottom - window.top);
            let fit_w = width.min(work.right - work.left);
            let fit_h = height.min(work.bottom - work.top);
            if fit_w == width && fit_h == height {
                return;
            }
            let x = window.left.clamp(work.left, work.right - fit_w);
            let y = window.top.clamp(work.top, work.bottom - fit_h);
            SetWindowPos(hwnd, 0, x, y, fit_w, fit_h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
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
            WM_GETDPISCALEDSIZE => {
                // SAFETY: lParam is an in/out SIZE for this message.
                if let Some(size) = unsafe { scaled_size(hwnd, wparam as u32) } {
                    unsafe { *(lparam as *mut SizeOut) = size };
                    return 1;
                }
            }
            WM_DPICHANGED => {
                // SAFETY: lParam points to the suggested RECT for this message.
                let suggested = unsafe { *(lparam as *const Rect) };
                let outer = SUGGESTED.replace(Some(suggested));
                let result = forward();
                SUGGESTED.set(outer);
                return result;
            }
            WM_WINDOWPOSCHANGING => {
                if let Some(rect) = SUGGESTED.get() {
                    // SAFETY: lParam is the WINDOWPOS the system is about to apply.
                    let pos = unsafe { &mut *(lparam as *mut WindowPos) };
                    if pos.flags & SWP_NOMOVE == 0 {
                        pos.x = rect.left;
                        pos.y = rect.top;
                    }
                    if pos.flags & SWP_NOSIZE == 0 {
                        pos.cx = rect.right - rect.left;
                        pos.cy = rect.bottom - rect.top;
                    }
                }
            }
            WM_GETMINMAXINFO => {
                let result = forward();
                // SAFETY: lParam is the MINMAXINFO being filled in.
                if let Some(min) = unsafe { min_track_size(hwnd, hook.min) } {
                    unsafe { (*(lparam as *mut MinMaxInfo)).min_track = min };
                }
                return result;
            }
            WM_EXITSIZEMOVE => {
                let result = forward();
                // SAFETY: the window is live while it handles messages.
                unsafe { fit_to_work_area(hwnd) };
                return result;
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

    #[cfg(test)]
    mod tests {
        use super::{is_frameless, rescale, scaled_client_size, Rect};

        #[test]
        fn rescale_matches_the_logical_round_trip() {
            assert_eq!(rescale(1153, 144, 96), 769);
            assert_eq!(rescale(769, 96, 144), 1154);
            assert_eq!(rescale(840, 144, 96), 560);
            assert_eq!(rescale(560, 96, 144), 840);
        }

        #[test]
        fn frameless_detection_compares_sizes_not_positions() {
            let client = Rect {
                left: 0,
                top: 0,
                right: 1080,
                bottom: 720,
            };
            let same_size = Rect {
                left: 200,
                top: 100,
                right: 1280,
                bottom: 820,
            };
            let framed = Rect {
                left: 200,
                top: 100,
                right: 1096,
                bottom: 759,
            };
            assert!(is_frameless(same_size, client));
            assert!(!is_frameless(framed, client));
        }

        #[test]
        fn frameless_scaling_does_not_add_a_phantom_frame() {
            let client = Rect {
                left: 0,
                top: 0,
                right: 1620,
                bottom: 1080,
            };
            let scaled = scaled_client_size(client, 144, 96);
            assert_eq!((scaled.cx, scaled.cy), (1080, 720));
            let back = scaled_client_size(
                Rect {
                    left: 0,
                    top: 0,
                    right: scaled.cx,
                    bottom: scaled.cy,
                },
                96,
                144,
            );
            assert_eq!((back.cx, back.cy), (1620, 1080));
        }
    }
}

#[cfg(not(all(windows, target_pointer_width = "64")))]
mod imp {
    pub fn install(_hwnd: isize, _min: (f32, f32)) -> bool {
        false
    }
}

/// Wraps the native window once it exists. Safe to call on every tick:
/// a window that is already wrapped is left alone, and a native window
/// recreated after hiding is wrapped again.
pub(super) fn ensure(window: &slint::Window, min: (f32, f32)) {
    if let Some(hwnd) = super::native::hwnd(window) {
        imp::install(hwnd, min);
    }
}

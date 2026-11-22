//! 共享的 Win32 窗口句柄读取，供 DPI 与窗口 chrome 的窗口过程使用。

#[cfg(windows)]
pub(super) fn hwnd(window: &slint::Window) -> Option<isize> {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    window
        .with_winit_window(|native| match native.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get()),
            _ => None,
        })
        .flatten()
}

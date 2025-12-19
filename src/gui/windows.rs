use std::cell::Cell;
use std::cmp::max;
use std::rc::Rc;
use std::sync::OnceLock;

use flume::Sender;
use gtk::glib;
use gtk::prelude::{GtkWindowExt, WidgetExt};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CWPRETSTRUCT, CallNextHookEx, GWL_STYLE, GetWindowInfo, GetWindowRect, HHOOK, HWND_TOP,
    MoveWindow, SET_WINDOW_POS_FLAGS, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW,
    SetWindowPos, SetWindowsHookExW, UnhookWindowsHookEx, WH_CALLWNDPROCRET, WINDOWINFO,
    WM_DPICHANGED, WM_WINDOWPOSCHANGED, WS_OVERLAPPED, WS_POPUP, WS_VISIBLE,
};

use super::Gui;
use crate::com::Res;

#[derive(Debug)]
struct SendHWND(HWND);

// HWNDs are generally thread-safe and are more IDs than pointers.
// How they are being used in this application is entirely safe.
unsafe impl Send for SendHWND {}
unsafe impl Sync for SendHWND {}

static WNDPROC_CHAN: OnceLock<Sender<Event>> = OnceLock::new();

static PRIMARY_HWND: OnceLock<SendHWND> = OnceLock::new();

#[derive(Debug)]
enum Event {
    Dpi(u16),
    PosChange,
}

unsafe extern "system" fn hook_callback(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    unsafe {
        if code < 0 {
            return CallNextHookEx(None, code, w_param, l_param);
        }

        if l_param.0 == 0 {
            // Shouldn't happen
            return CallNextHookEx(None, code, w_param, l_param);
        }

        let params = l_param.0 as *const CWPRETSTRUCT;
        let p = &*params;
        if p.hwnd != PRIMARY_HWND.get().unwrap().0 {
            return CallNextHookEx(None, code, w_param, l_param);
        }

        if p.message == WM_WINDOWPOSCHANGED {
            // let pos = unsafe { &*(p.lParam.0 as *const WINDOWPOS) };
            drop(WNDPROC_CHAN.get().unwrap().send(Event::PosChange));
        } else if p.message == WM_DPICHANGED {
            drop(WNDPROC_CHAN.get().unwrap().send(Event::Dpi(p.wParam.0 as u16)));
        }

        CallNextHookEx(None, code, w_param, l_param)
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct WinState {
    // How big the window was. In the future this may be adjusted for monitor res.
    size: Res,
    // The margins to the left and top as a percentage of the monitor res MINUS the window res.
    // Valid range is [0, 1.0)
    margins: (f32, f32),
    maximized: bool,
}


#[derive(Debug, Default)]
pub struct WindowsEx {
    dpi: Cell<u16>,
    fullscreen: Cell<bool>,
    saved_state: Cell<WinState>,
    hwnd: OnceLock<HWND>,
    hook: OnceLock<HHOOK>,
}

impl WindowsEx {
    pub(super) fn setup(&self, g: Rc<Gui>) {
        let (s, r) = flume::unbounded();
        WNDPROC_CHAN.set(s).unwrap();

        unsafe {
            self.hwnd.set(GetActiveWindow()).unwrap();
            let hwnd = *self.hwnd.get().unwrap();
            PRIMARY_HWND.set(SendHWND(hwnd)).unwrap();

            let hhook = SetWindowsHookExW(
                WH_CALLWNDPROCRET,
                Some(hook_callback),
                None,
                GetCurrentThreadId(),
            )
            .unwrap();
            self.hook.set(hhook).unwrap();

            let dpi = GetDpiForWindow(hwnd);
            self.set_dpi(&g, dpi as u16);
        }

        let ctx = glib::MainContext::ref_thread_default();
        ctx.spawn_local_with_priority(glib::Priority::DEFAULT, async move {
            while let Ok(e) = r.recv_async().await {
                g.windows_event(e);
            }
        });
    }

    fn set_dpi(&self, g: &Gui, dpi: u16) {
        if dpi != self.dpi.get() {
            debug!("New DPI {dpi}");
            self.dpi.set(dpi);
            g.window.remove_css_class("dpi100");
            g.window.remove_css_class("dpi125");
            g.window.remove_css_class("dpi150");
            g.window.remove_css_class("dpi175");
            g.window.remove_css_class("dpi200");

            g.window.add_css_class(self.dpi_class());
        }
    }

    pub(super) fn dpi_class(&self) -> &str {
        match self.dpi.get() {
            0..=108 => "dpi100",
            109..=132 => "dpi125",
            133..=156 => "dpi150",
            157..=180 => "dp175",
            181.. => "dpi200",
        }
    }

    pub(super) fn is_fullscreen(&self) -> bool {
        self.fullscreen.get()
    }

    pub(super) fn fullscreen(&self, g: &Gui) {
        unsafe {
            let hwnd = *self.hwnd.get().unwrap();

            let mut pos = RECT::default();
            if let Err(e) = GetWindowRect(hwnd, &mut pos) {
                error!("GetWindowRect: {e}");
            }

            let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };

            if !GetMonitorInfoW(hmonitor, &mut info).as_bool() {
                error!("GetMonitorInfoW: failed");
            }

            let mpos = &info.rcMonitor;

            let size: Res = (pos.right - pos.left, pos.bottom - pos.top).into();
            let (w, h) = (max(mpos.right - mpos.left, 1), max(mpos.bottom - mpos.top, 1));

            let margins = (
                ((pos.left - mpos.left) as f32 / w as f32).clamp(0.0, 1.0),
                ((pos.top - mpos.top) as f32 / h as f32).clamp(0.0, 1.0),
            );

            let maximized = g.window.is_maximized();

            self.saved_state.set(WinState { size, margins, maximized });
        }

        self.fullscreen.set(true);
        g.window.fullscreen();
    }

    pub(super) fn unfullscreen(&self, g: &Rc<Gui>) {
        let state = self.saved_state.get();
        g.window.unfullscreen();

        unsafe {
            let hwnd = *self.hwnd.get().unwrap();

            let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };

            if !GetMonitorInfoW(hmonitor, &mut info).as_bool() {
                error!("GetMonitorInfoW: failed");
            }

            let mpos = &info.rcMonitor;

            let (mx, my) = (
                (state.margins.0 * (mpos.right - mpos.left) as f32).round() as i32,
                (state.margins.1 * (mpos.bottom - mpos.top) as f32).round() as i32,
            );

            let (w, h) = (state.size.w as i32, state.size.h as i32);

            if let Err(e) = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                mpos.left + mx,
                mpos.top + my,
                w,
                h,
                SET_WINDOW_POS_FLAGS::default(),
            ) {
                error!("SetWindowPos: {e}");
            }

            if state.maximized {
                g.window.unmaximize();
                g.window.maximize();
            }
        }
        // We're still "in" the fullscreen state until the above maximize changes finish.
        // This might suppress changes in window_state_changed if we care about updating something
        // when exiting fullscreen, but for now it's acceptable.
        self.fullscreen.set(false);
    }

    pub(super) fn teardown(&self) {
        let hhook = self.hook.get().unwrap();
        unsafe {
            if let Err(e) = UnhookWindowsHookEx(*hhook) {
                error!("UnhookWindowsHookEx: {e}");
            }
        }
    }
}

impl Gui {
    fn windows_event(self: &Rc<Self>, e: Event) {
        match e {
            Event::Dpi(dpi) => self.win32.set_dpi(self, dpi),
            Event::PosChange => {
                if !self.win32.fullscreen.get() {
                    return;
                }

                if !self.window.is_fullscreen() {
                    debug!("Completing re-fullscreen");
                    self.window.fullscreen();
                    return;
                }

                let hwnd = *self.win32.hwnd.get().unwrap();

                unsafe {
                    let mut info = WINDOWINFO {
                        cbSize: std::mem::size_of::<WINDOWINFO>() as u32,
                        ..Default::default()
                    };
                    let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut minfo = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };

                    if let Err(e) = GetWindowInfo(hwnd, &mut info) {
                        error!("GetWindowInfo: {e}");
                    }
                    if !GetMonitorInfoW(hmonitor, &mut minfo).as_bool() {
                        error!("GetWindowInfoW: failed");
                    }

                    if info.dwStyle.contains(WS_POPUP) {
                        debug!("Trying to correct fullscreen style.");

                        SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_OVERLAPPED | WS_VISIBLE).0 as isize);

                        if let Err(e) = SetWindowPos(
                            hwnd,
                            None,
                            0,
                            0,
                            0,
                            0,
                            SWP_FRAMECHANGED | SWP_NOSIZE | SWP_NOMOVE,
                        ) {
                            error!("SetWindowPos: {e}");
                        }
                    }

                    let pos = info.rcWindow;
                    let mpos = &minfo.rcMonitor;
                    if pos.right - pos.left < mpos.right - mpos.left
                        || pos.bottom - pos.top < mpos.bottom - mpos.top
                    {
                        debug!("Trying to correct fullscreen resolution and monitor.");

                        let g = self.clone();
                        g.window.unfullscreen();
                        if let Err(e) = MoveWindow(
                            hwnd,
                            mpos.left,
                            mpos.top,
                            mpos.right - mpos.left,
                            mpos.bottom - mpos.top,
                            false,
                        ) {
                            error!("MoveWindow: {e}");
                        }
                    }
                }
            }
        }
    }
}

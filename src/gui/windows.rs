use std::cell::Cell;
use std::cmp::max;
use std::rc::Rc;

use gtk::glib;
use gtk::traits::{GtkWindowExt, WidgetExt};
use once_cell::unsync::OnceCell;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetWindowInfo, GetWindowRect, MoveWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowsHookExW, UnhookWindowsHookEx, CWPRETSTRUCT, GWL_STYLE, HHOOK, HWND_TOP,
    SET_WINDOW_POS_FLAGS, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, WH_CALLWNDPROCRET, WINDOWINFO,
    WM_DPICHANGED, WM_WINDOWPOSCHANGED, WS_OVERLAPPED, WS_POPUP, WS_VISIBLE,
};

use super::Gui;
use crate::com::Res;

static WNDPROC_CHAN: once_cell::sync::OnceCell<glib::Sender<Event>> =
    once_cell::sync::OnceCell::new();

static PRIMARY_HWND: once_cell::sync::OnceCell<HWND> = once_cell::sync::OnceCell::new();

#[derive(Debug)]
enum Event {
    Dpi(u16),
    PosChange,
}

unsafe extern "system" fn hook_callback(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(HHOOK(0), code, w_param, l_param);
    }

    if l_param.0 == 0 {
        // Shouldn't happen
        return CallNextHookEx(HHOOK(0), code, w_param, l_param);
    }

    let params = l_param.0 as *const CWPRETSTRUCT;
    let p = unsafe { &*params };
    if p.hwnd != *PRIMARY_HWND.get().unwrap() {
        return CallNextHookEx(HHOOK(0), code, w_param, l_param);
    }

    if p.message == WM_WINDOWPOSCHANGED {
        // let pos = unsafe { &*(p.lParam.0 as *const WINDOWPOS) };
        drop(WNDPROC_CHAN.get().unwrap().send(Event::PosChange));
    } else if p.message == WM_DPICHANGED {
        drop(WNDPROC_CHAN.get().unwrap().send(Event::Dpi(p.wParam.0 as u16)));
    }

    CallNextHookEx(HHOOK(0), code, w_param, l_param)
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
    hwnd: OnceCell<HWND>,
    hook: OnceCell<HHOOK>,
}

impl WindowsEx {
    pub(super) fn setup(&self, g: Rc<Gui>) {
        let (s, r) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
        WNDPROC_CHAN.set(s).unwrap();

        unsafe {
            self.hwnd.set(GetActiveWindow()).unwrap();
            let hwnd = *self.hwnd.get().unwrap();
            PRIMARY_HWND.set(hwnd).unwrap();

            let hhook = SetWindowsHookExW(
                WH_CALLWNDPROCRET,
                Some(hook_callback),
                HINSTANCE::default(),
                GetCurrentThreadId(),
            )
            .unwrap();
            self.hook.set(hhook).unwrap();

            let dpi = GetDpiForWindow(hwnd);
            self.set_dpi(&g, dpi as u16);
        }

        r.attach(None, move |e| {
            g.windows_event(e);
            glib::Continue(true)
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
        // TODO -- needs to remember position before being maximized, not just before being
        // fullscreened.
        unsafe {
            let hwnd = *self.hwnd.get().unwrap();

            let mut pos = RECT::default();
            GetWindowRect(hwnd, &mut pos);

            let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };

            GetMonitorInfoW(hmonitor, &mut info);

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

            GetMonitorInfoW(hmonitor, &mut info);

            let mpos = &info.rcMonitor;

            let (mx, my) = (
                (state.margins.0 * (mpos.right - mpos.left) as f32).round() as i32,
                (state.margins.1 * (mpos.bottom - mpos.top) as f32).round() as i32,
            );

            let (w, h) = (state.size.w as i32, state.size.h as i32);

            SetWindowPos(
                hwnd,
                HWND_TOP,
                mpos.left + mx,
                mpos.top + my,
                w,
                h,
                SET_WINDOW_POS_FLAGS::default(),
            );

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
            UnhookWindowsHookEx(*hhook);
        }
    }
}

impl Gui {
    fn windows_event(self: &Rc<Self>, e: Event) {
        match e {
            Event::Dpi(dpi) => self.win32.set_dpi(&self, dpi),
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

                    GetWindowInfo(hwnd, &mut info);
                    GetMonitorInfoW(hmonitor, &mut minfo);

                    if info.dwStyle & WS_POPUP.0 != 0 {
                        debug!("Trying to correct fullscreen style.");

                        SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_OVERLAPPED | WS_VISIBLE).0 as isize);

                        SetWindowPos(
                            hwnd,
                            HWND::default(),
                            0,
                            0,
                            0,
                            0,
                            SWP_FRAMECHANGED | SWP_NOSIZE | SWP_NOMOVE,
                        );
                    }

                    let pos = info.rcWindow;
                    let mpos = &minfo.rcMonitor;
                    if pos.right - pos.left < mpos.right - mpos.left
                        || pos.bottom - pos.top < mpos.bottom - mpos.top
                    {
                        debug!("Trying to correct fullscreen resolution and monitor.");

                        let g = self.clone();
                        g.window.unfullscreen();
                        MoveWindow(
                            hwnd,
                            mpos.left,
                            mpos.top,
                            mpos.right - mpos.left,
                            mpos.bottom - mpos.top,
                            false,
                        );
                    }
                }
            }
        }
    }
}

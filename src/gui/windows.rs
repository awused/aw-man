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
    GetWindowRect, SetWindowPos, SetWindowsHookExW, UnhookWindowsHookEx, CWPRETSTRUCT, HHOOK,
    HWND_TOP, SET_WINDOW_POS_FLAGS, WH_CALLWNDPROCRET, WINDOWPOS, WM_DPICHANGED,
    WM_WINDOWPOSCHANGED,
};

use super::Gui;
use crate::com::Res;

static WNDPROC_CHAN: once_cell::sync::OnceCell<glib::Sender<Event>> =
    once_cell::sync::OnceCell::new();

unsafe extern "system" fn hook_callback(_code: i32, _w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    // Shouldn't happen
    if l_param.0 == 0 {
        return LRESULT::default();
    }

    let params = l_param.0 as *const CWPRETSTRUCT;
    let p = unsafe { &*params };

    if p.message == WM_WINDOWPOSCHANGED {
        let pos = unsafe { &*(p.lParam.0 as *const WINDOWPOS) };
        // println!("pos changed, {pos:?}");
        println!("position: {:?}", pos);
        drop(WNDPROC_CHAN.get().unwrap().send(Event::PosChange));
    } else if p.message == WM_DPICHANGED {
        println!("dpi changed: {} {}", p.wParam.0 as u16, (p.wParam.0 >> 16) as u16);
        drop(WNDPROC_CHAN.get().unwrap().send(Event::Dpi(p.wParam.0 as u16)));
    }

    // Should call next hook here. For now, don't bother.
    LRESULT::default()
}

#[derive(Debug)]
enum Event {
    Dpi(u16),
    PosChange,
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
    pub fn setup(&self, g: Rc<Gui>) {
        let (s, r) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
        WNDPROC_CHAN.set(s).unwrap();

        unsafe {
            self.hwnd.set(GetActiveWindow()).unwrap();
            let hwnd = *self.hwnd.get().unwrap();

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

        r.attach(None, move |arg| {
            match arg {
                Event::Dpi(dpi) => g.win32.set_dpi(&g, dpi),
                Event::PosChange => {
                    if g.win32.fullscreen.get() {
                        println!("React to fullscreen pos change");
                    }
                }
            }

            glib::Continue(true)
        });
    }

    fn set_dpi(&self, g: &Gui, dpi: u16) {
        if dpi != self.dpi.get() {
            self.dpi.set(dpi);
            g.window.remove_css_class("dpi100");
            g.window.remove_css_class("dpi125");
            g.window.remove_css_class("dpi150");
            g.window.remove_css_class("dpi175");
            g.window.remove_css_class("dpi200");

            // 25% of 96 is 24, half of that is 12.
            let class = match dpi {
                0..=108 => "dpi100",
                109..=132 => "dpi125",
                133..=156 => "dpi150",
                157..=180 => "dp175",
                181.. => "dpi200",
            };

            g.window.add_css_class(class);
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen.get()
    }

    pub fn fullscreen(&self, g: &Gui) {
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

            SetWindowPos(
                hwnd,
                HWND_TOP,
                mpos.left,
                mpos.top,
                w,
                h,
                SET_WINDOW_POS_FLAGS::default(),
            );


            self.saved_state.set(WinState { size, margins, maximized });
        }

        self.fullscreen.set(true);
    }

    pub fn unfullscreen(&self, g: &Rc<Gui>) {
        let state = self.saved_state.get();

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

            // TODO -- confirm this.
            if state.maximized && !g.window.is_maximized() {
                g.window.maximize();
            }
        }

        self.fullscreen.set(false);
    }

    pub fn teardown(&self) {
        let hhook = self.hook.get().unwrap();
        unsafe {
            UnhookWindowsHookEx(*hhook);
        }
    }
}

use std::rc::Rc;

use gtk::glib;
use gtk::traits::WidgetExt;
use once_cell::unsync::OnceCell;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowsHookExW, UnhookWindowsHookEx, CWPRETSTRUCT, HHOOK, WH_CALLWNDPROCRET, WINDOWPOS,
    WM_ACTIVATEAPP, WM_DPICHANGED, WM_EXITSIZEMOVE, WM_MOVE, WM_WINDOWPOSCHANGED,
};

use super::Gui;

static WNDPROC_CHAN: once_cell::sync::OnceCell<glib::Sender<usize>> =
    once_cell::sync::OnceCell::new();

unsafe extern "system" fn hook_callback(_code: i32, _w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    // Shouldn't happen
    if l_param.0 == 0 {
        return LRESULT::default();
    }

    let params = l_param.0 as *const CWPRETSTRUCT;
    let p = unsafe { &*params };

    if p.message == WM_ACTIVATEAPP {
        println!("isActive: {}", p.wParam.0 != 0);
    } else if p.message == WM_MOVE {
        println!("move: x: {} y {}", p.lParam.0 & 0xffff, (p.lParam.0 >> 32) & 0xffff)
    } else if p.message == WM_EXITSIZEMOVE {
        println!("exit sizemove");
    } else if p.message == WM_WINDOWPOSCHANGED {
        let pos = unsafe { &*(p.lParam.0 as *const WINDOWPOS) };
        println!("pos changed, {pos:?}");
    } else if p.message == WM_DPICHANGED {
        println!("dpi changed: {} {}", p.wParam.0 & 0xffff, (p.wParam.0 >> 32) & 0xffff);
    }
    // WM_DISPLAYCHANGE
    //

    // println!("{p:#?}");
    // WNDPROC_CHAN.get().unwrap().send(p.message as usize).unwrap();
    // Should call next hook here. For now, don't bother.
    LRESULT::default()
}


// #[cfg(windows)]
#[derive(Debug, Default)]
pub struct WindowsEx {
    dpi: usize,
    base_font: usize,
    hwnd: OnceCell<HWND>,
    hook: OnceCell<HHOOK>,
}

impl WindowsEx {
    pub fn setup(&self, g: Rc<Gui>) {
        let (s, r) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
        WNDPROC_CHAN.set(s).unwrap();
        r.attach(None, move |arg| {
            println!("{arg}");
            glib::Continue(true)
        });

        println!("{:?}", gtk::Settings::default().unwrap().gtk_titlebar_double_click());
        // gtk::Settings::default()
        //     .unwrap()
        //     .set_gtk_titlebar_double_click(Some("toggle-maximize"));

        // g.window.add_css_class("dpi100");
        // g.window.add_css_class("dpi125");
        g.window.add_css_class("dpi150");

        unsafe {
            self.hwnd.set(GetActiveWindow()).unwrap();
            // let hwnd = self.hwnd.get().unwrap();

            let hhook = SetWindowsHookExW(
                WH_CALLWNDPROCRET,
                Some(hook_callback),
                HINSTANCE::default(),
                GetCurrentThreadId(),
            )
            .unwrap();
            self.hook.set(hhook).unwrap();

            // TODO -- it's not terribly likely we'd want to pass hwnd to Status commands, but
            // it should be possible.
            // WINDOW_ID.set(hwnd.to_bits().to_string()).unwrap();

            // TODO
            // Handle initial DPI
            // set_fullscreen
            //
        }
    }

    pub fn set_fullscreen() {
        // Save size, position, and display
        // If display differs when removing fullscreen, handle by not restoring position exactly.

        // unsafe {
        //     use winapi::um::winuser::*;
        //
        //     let hwnd = *self.win32.hwnd.get().unwrap();
        //     let mon_handle = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        //     let mut info = MONITORINFOEXW {
        //         cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
        //         ..Default::default()
        //     };
        //
        //     GetMonitorInfoW(mon_handle, &mut info as *mut _ as _);
        //
        //     let r = info.rcMonitor;
        //     // TODO -- here
        //     self.window.set_decorated(false);
        //     self.window.add_css_class("nodecorations");
        //     self.window.set_size_request(r.right - r.left, r.bottom - r.top);
        //
        //     SetWindowPos(
        //         hwnd,
        //         HWND_TOP,
        //         r.left,
        //         r.top,
        //         r.right - r.left,
        //         r.bottom - r.top,
        //         SWP_FRAMECHANGED,
        //     );
        //
        //     let g = self.clone();
        //     // Listen for changes and readjust self if monitor is different.
        //     // Layout fires a lot, this is wasteful, but I can't override
        //     // WndProc and other events don't
        //     // seem to be reliable.
        //     self.window.surface().connect_layout(move |a, b, c| {
        //         let start = Instant::now();
        //
        //
        //         let hwnd = *g.win32.hwnd.get().unwrap();
        //         let mon_handle =
        //             MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        //         let mut info = MONITORINFOEXW {
        //             cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
        //             ..Default::default()
        //         };
        //
        //         GetMonitorInfoW(mon_handle, &mut info as *mut _ as _);
        //         let r = info.rcMonitor;
        //
        //         println!("layout {b:?} {r:?}, {:?}", start.elapsed());
        //     });
        //
        // return;
    }

    pub fn teardown(&self) {
        let hhook = self.hook.get().unwrap();
        unsafe {
            UnhookWindowsHookEx(*hhook);
        }
    }
}

//! 全局热键（RegisterHotKey + 线程消息循环）。
//! 供 CLI 使用；Tauri 版优先用 tauri-plugin-global-shortcut。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, HOT_KEY_MODIFIERS};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PostThreadMessageW, MSG, WM_HOTKEY, WM_QUIT,
};

pub const MOD_NOREPEAT: u32 = 0x4000;
pub const VK_F6: u32 = 0x75;
pub const VK_F7: u32 = 0x76;

/// (id, modifiers, vk)：id 用于回调中区分热键
pub struct HotkeyListener {
    stop: Arc<AtomicBool>,
    thread_id: Arc<Mutex<u32>>,
    handle: Option<JoinHandle<()>>,
}

impl HotkeyListener {
    pub fn spawn(
        bindings: Vec<(i32, u32, u32)>,
        on_hotkey: impl Fn(i32) + Send + Sync + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(Mutex::new(0u32));
        let stop_t = stop.clone();
        let tid_t = thread_id.clone();
        let handle = std::thread::spawn(move || unsafe {
            *tid_t.lock().unwrap() = GetCurrentThreadId();
            for (id, mods, vk) in &bindings {
                let _ = RegisterHotKey(None, *id, HOT_KEY_MODIFIERS(*mods), *vk);
            }
            let mut msg = MSG::default();
            loop {
                // 阻塞等热键消息；stop() 通过 PostThreadMessage(WM_QUIT) 唤醒
                if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                    break;
                }
                if msg.message == WM_HOTKEY {
                    on_hotkey(msg.wParam.0 as i32);
                }
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
            }
        });
        Self {
            stop,
            thread_id,
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let tid = *self.thread_id.lock().unwrap();
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.stop();
    }
}

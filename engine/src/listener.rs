//! 全局键盘低级钩子监听（半自动模式与录制共用）。
//! 结构参考 wwcombo `winhook`：WH_KEYBOARD_LL + 消息循环线程；
//! 额外增加 `LLKHF_INJECTED` 过滤——只上报真人物理按键，
//! 程序注入的输入（含本播放器发出的键）不上报，避免自触发。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

pub const VK_DIGIT1: u32 = 0x31;
pub const VK_DIGIT4: u32 = 0x34;

/// 数字键 1-4 → 角色槽位
pub fn slot_of(vk: u32) -> Option<u8> {
    match vk {
        VK_DIGIT1..=VK_DIGIT4 => Some((vk - VK_DIGIT1 + 1) as u8),
        _ => None,
    }
}

/// 钩子回调（安装线程的消息循环内被调用）→ 监听线程的 channel。
/// 回调里只 try_lock 快速投递，绝不阻塞钩子链。
static SENDER: OnceLock<Mutex<Option<Sender<u32>>>> = OnceLock::new();

unsafe extern "system" fn keyboard_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    const HC_ACTION: i32 = 0;
    const LLKHF_INJECTED: u32 = 0x10;
    let wp = wparam.0 as u32;
    if ncode == HC_ACTION && (wp == WM_KEYDOWN || wp == WM_SYSKEYDOWN) {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if kb.flags.0 & LLKHF_INJECTED == 0 {
            if let Some(m) = SENDER.get() {
                if let Ok(guard) = m.try_lock() {
                    if let Some(tx) = guard.as_ref() {
                        let _ = tx.send(kb.vkCode as u32);
                    }
                }
            }
        }
    }
    CallNextHookEx(None, ncode, wparam, lparam)
}

/// 全局键盘监听器：`recv()` 收到真人物理 keydown 的虚拟键码
pub struct KeyListener {
    stop: Arc<AtomicBool>,
    thread_id: Arc<Mutex<u32>>,
    handle: Option<JoinHandle<()>>,
}

impl KeyListener {
    pub fn spawn() -> (Self, mpsc::Receiver<u32>) {
        let (tx, rx) = mpsc::channel::<u32>();
        let cell = SENDER.get_or_init(|| Mutex::new(None));
        *cell.lock().unwrap() = Some(tx);

        let stop = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(Mutex::new(0u32));
        let stop_t = stop.clone();
        let tid_t = thread_id.clone();
        let handle = thread::spawn(move || unsafe {
            let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0) else {
                eprintln!("安装键盘钩子失败: {:?}", std::io::Error::last_os_error());
                return;
            };
            *tid_t.lock().unwrap() = windows::Win32::System::Threading::GetCurrentThreadId();
            let mut msg = MSG::default();
            loop {
                if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                    break;
                }
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
            }
            let _ = UnhookWindowsHookEx(hook);
            // 清理 sender，钩子已卸载不会再触发
            if let Some(m) = SENDER.get() {
                if let Ok(mut guard) = m.try_lock() {
                    *guard = None;
                }
            }
        });
        (
            Self {
                stop,
                thread_id,
                handle: Some(handle),
            },
            rx,
        )
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

impl Drop for KeyListener {
    fn drop(&mut self) {
        self.stop();
    }
}

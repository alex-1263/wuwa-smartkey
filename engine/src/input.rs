//! Win32 输入注入：键盘扫描码 + 鼠标按键（SendInput）。
//! 引擎内所有输入必须经过本模块：
//! - 按住追踪：任何路径按下的键都会记录，`release_all` 用于紧急停止时强制抬键
//! - 可中断等待：播放器停止信号能打断任意等待/按压，且按压保证 down/up 配对

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};

// 扫描码（Set 1）
pub const SC_1: u16 = 0x02;
pub const SC_2: u16 = 0x03;
pub const SC_3: u16 = 0x04;
pub const SC_4: u16 = 0x05;
pub const SC_E: u16 = 0x12;
pub const SC_Q: u16 = 0x10;
pub const SC_R: u16 = 0x13;
pub const SC_T: u16 = 0x14;
pub const SC_F: u16 = 0x21;
pub const SC_LSHIFT: u16 = 0x2A;
pub const SC_SPACE: u16 = 0x39;

/// 默认按键时长（点按），ms
pub const DEFAULT_TAP_MS: u64 = 40;
/// 切人后的等待时长，ms
pub const SWITCH_DELAY_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Device {
    Key(u16),
    MouseLeft,
    MouseRight,
}

fn send_key(scan: u16, up: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

fn send_mouse(flags: MOUSE_EVENT_FLAGS) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

pub fn dev_down(dev: Device) {
    match dev {
        Device::Key(scan) => send_key(scan, false),
        Device::MouseLeft => send_mouse(MOUSEEVENTF_LEFTDOWN),
        Device::MouseRight => send_mouse(MOUSEEVENTF_RIGHTDOWN),
    }
}

pub fn dev_up(dev: Device) {
    match dev {
        Device::Key(scan) => send_key(scan, true),
        Device::MouseLeft => send_mouse(MOUSEEVENTF_LEFTUP),
        Device::MouseRight => send_mouse(MOUSEEVENTF_RIGHTUP),
    }
}

/// 当前处于按下状态的设备（跨线程共享）
static HELD: Mutex<Vec<Device>> = Mutex::new(Vec::new());

fn track(dev: Device, down: bool) {
    let mut held = HELD.lock().unwrap();
    if down {
        if !held.contains(&dev) {
            held.push(dev);
        }
    } else {
        held.retain(|d| *d != dev);
    }
}

/// 强制抬起所有处于按下状态的设备（紧急停止兜底）
pub fn release_all() {
    let stuck: Vec<Device> = HELD.lock().unwrap().drain(..).collect();
    for dev in stuck {
        dev_up(dev);
    }
}

/// 可中断睡眠，返回 false 表示收到停止信号
pub fn sleep_interruptible(ms: u64, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return true;
        }
        std::thread::sleep(remain.min(Duration::from_millis(10)));
    }
}

/// 等到 t0 + target_ms，返回 false 表示收到停止信号
pub fn wait_until_interruptible(t0: Instant, target_ms: i64, stop: &AtomicBool) -> bool {
    let target = t0 + Duration::from_millis(target_ms.max(0) as u64);
    let now = Instant::now();
    if target <= now {
        return !stop.load(Ordering::SeqCst);
    }
    sleep_interruptible((target - now).as_millis() as u64, stop)
}

/// 按下 → 可中断保持 → 抬起。无论是否被中断，保证 up 配对
pub fn press_interruptible(dev: Device, hold_ms: u64, stop: &AtomicBool) -> bool {
    dev_down(dev);
    track(dev, true);
    let ok = sleep_interruptible(hold_ms, stop);
    dev_up(dev);
    track(dev, false);
    ok
}

/// 按下 → 保持 hold → 抬起（阻塞，不可中断）
pub fn press(dev: Device, hold: Duration) {
    dev_down(dev);
    track(dev, true);
    std::thread::sleep(hold);
    dev_up(dev);
    track(dev, false);
}

/// 招式 → 输入设备的默认映射（对应 wwcombo defaults.ts 键鼠表，未含双绑定的右键闪避）
pub fn default_binding(move_id: &str) -> Option<Device> {
    let dev = match move_id {
        "basic_attack" | "heavy_attack" => Device::MouseLeft,
        "skill" | "skill_hold" => Device::Key(SC_E),
        "echo" | "echo_hold" => Device::Key(SC_Q),
        "liberation" | "liberation_hold" => Device::Key(SC_R),
        "tool" => Device::Key(SC_T),
        "dodge" | "dodge_hold" => Device::Key(SC_LSHIFT),
        "jump" | "jump_hold" => Device::Key(SC_SPACE),
        "switch_1" => Device::Key(SC_1),
        "switch_2" => Device::Key(SC_2),
        "switch_3" => Device::Key(SC_3),
        "switch_4" => Device::Key(SC_4),
        "finisher" => Device::Key(SC_F),
        _ => return None,
    };
    Some(dev)
}

/// 长按类招式（按压时长有游戏内语义）
pub fn is_hold_move(move_id: &str) -> bool {
    move_id.ends_with("_hold") || move_id == "heavy_attack"
}

/// 提升系统定时器精度（播放期间持有）
pub fn begin_high_resolution_timer() {
    unsafe { windows::Win32::Media::timeBeginPeriod(1) };
}

pub fn end_high_resolution_timer() {
    unsafe { windows::Win32::Media::timeEndPeriod(1) };
}

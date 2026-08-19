//! wuwa-smartkey 引擎：轴解析、输入注入、播放调度、热键、轴库。
//! 本 crate 不依赖 Tauri，保持纯净，可独立测试。

pub mod chart;
pub mod hotkey;
pub mod input;
pub mod listener;
pub mod scheduler;
pub mod store;

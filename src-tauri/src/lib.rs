//! Tauri 应用层：commands 接引擎、全局热键（F6/F7）、播放事件推送前端。
#![allow(linker_messages)] // MSVC 生成 dll 导入库的正常输出，无需警告

use std::sync::Mutex;

use engine::scheduler::{Playback, PlaybackEvent, PlaybackMode, PlaybackOptions};
use engine::store;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
struct AppState {
    playing: Mutex<Option<Playback>>,
    /// 前端当前选中的轴（热键 F6 直接播放它）
    current: Mutex<Option<String>>,
}

fn do_start(
    app: &AppHandle,
    chart: engine::chart::ComboChart,
    opts: PlaybackOptions,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut playing = state.playing.lock().unwrap();
    if playing.is_some() {
        return Err("已在播放中".into());
    }
    let app2 = app.clone();
    *playing = Some(Playback::spawn(chart, opts, move |ev: PlaybackEvent| {
        let _ = app2.emit("playback-event", &ev);
    }));
    Ok(())
}

fn do_stop(app: &AppHandle) {
    let taken = {
        let state = app.state::<AppState>();
        let taken = state.playing.lock().unwrap().take();
        taken
    };
    if let Some(mut p) = taken {
        p.stop();
    }
}

#[tauri::command]
fn list_charts() -> Vec<store::ChartMeta> {
    store::list_charts()
}

#[tauri::command]
fn import_chart(path: String) -> Result<store::ChartMeta, String> {
    store::import_from(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_chart(file: String) -> Result<(), String> {
    store::delete(&file).map_err(|e| e.to_string())
}

/// 前端选中轴时同步到后端，供 F6 热键直接播放
#[tauri::command]
fn set_current_chart(app: AppHandle, file: Option<String>) {
    *app.state::<AppState>().current.lock().unwrap() = file;
}

#[tauri::command]
fn start_playback(
    app: AppHandle,
    file: String,
    loops: Option<u32>,
    mode: String,
    dry_run: bool,
) -> Result<(), String> {
    let chart = store::load_chart(&file).map_err(|e| format!("读取轴失败: {e}"))?;
    let mode = match mode.as_str() {
        "startup" => PlaybackMode::StartupOnly,
        "loop" => PlaybackMode::LoopOnly,
        _ => PlaybackMode::Full,
    };
    let opts = PlaybackOptions {
        mode,
        max_loops: loops,
        dry_run,
        ..Default::default()
    };
    do_start(&app, chart, opts)
}

#[tauri::command]
fn stop_playback(app: AppHandle) {
    do_stop(&app);
}

#[tauri::command]
fn playback_status(app: AppHandle) -> bool {
    app.state::<AppState>().playing.lock().unwrap().is_some()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_shortcuts(["F6", "F7", "F8"])
                .expect("注册全局热键失败")
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    match shortcut.to_string().as_str() {
                        // 开始逻辑放前端：倒计时（可取消）后再调 start_playback
                        "F6" => {
                            let _ = app.emit("hotkey", "start");
                        }
                        "F7" => {
                            do_stop(app);
                            let _ = app.emit("hotkey", "stop");
                        }
                        // 快速重同步：停止当前播放，从循环轴第一拍重开
                        "F8" => {
                            do_stop(app);
                            let _ = app.emit("hotkey", "restart-loop");
                        }
                        _ => {}
                    }
                })
                .build(),
        )
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            list_charts,
            import_chart,
            delete_chart,
            set_current_chart,
            start_playback,
            stop_playback,
            playback_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

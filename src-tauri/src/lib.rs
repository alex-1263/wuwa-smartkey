//! Tauri 应用层：commands 接引擎、全局热键（F6/F7）、播放事件推送前端。
#![allow(linker_messages)] // MSVC 生成 dll 导入库的正常输出，无需警告

use std::sync::Mutex;

use engine::chart::ComboChart;
use engine::scheduler::{Playback, PlaybackEvent, PlaybackMode, PlaybackOptions};
use engine::store;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const DEFAULT_HOTKEYS: (&str, &str, &str) = store::DEFAULT_HOTKEYS;

/// 按当前设置（未配置回填默认 F6/F7/F8）注册三个全局热键
fn register_hotkeys(app: &AppHandle) -> Result<(), String> {
    let s = store::load_settings();
    let keys = [
        s.hotkey_start.clone(),
        s.hotkey_stop.clone(),
        s.hotkey_restart.clone(),
    ];
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    for key in keys.iter().flatten() {
        let sc: Shortcut = key
            .parse()
            .map_err(|e| format!("无效热键 {key}: {e:?}"))?;
        gs.register(sc)
            .map_err(|e| format!("注册热键 {key} 失败: {e}"))?;
    }
    Ok(())
}

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

/// 完整轴数据（可视化用）
#[tauri::command]
fn get_chart(file: String) -> Result<ComboChart, String> {
    store::load_chart(&file).map_err(|e| e.to_string())
}

/// 编辑器保存：无损更新步骤时间
#[tauri::command]
fn update_steps(
    file: String,
    patches: Vec<engine::chart::StepPatch>,
) -> Result<usize, String> {
    store::patch_steps(&file, &patches).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_settings() -> store::Settings {
    store::load_settings()
}

#[tauri::command]
fn set_settings(app: AppHandle, settings: store::Settings) -> Result<(), String> {
    store::save_settings(&settings).map_err(|e| e.to_string())?;
    register_hotkeys(&app)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    // 与设置中的热键匹配（热键可由用户自定义）
                    let s = store::load_settings();
                    let key = shortcut.to_string();
                    if Some(&key) == s.hotkey_start.as_ref() {
                        // 开始逻辑放前端：倒计时（可取消）后再调 start_playback
                        let _ = app.emit("hotkey", "start");
                    } else if Some(&key) == s.hotkey_stop.as_ref() {
                        do_stop(app);
                        let _ = app.emit("hotkey", "stop");
                    } else if Some(&key) == s.hotkey_restart.as_ref() {
                        // 快速重同步：停止当前播放，从循环轴第一拍重开
                        do_stop(app);
                        let _ = app.emit("hotkey", "restart-loop");
                    }
                })
                .build(),
        )
        .setup(|app| {
            if let Err(e) = register_hotkeys(app.handle()) {
                eprintln!("热键注册失败: {e}");
            }
            Ok(())
        })
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            list_charts,
            import_chart,
            delete_chart,
            set_current_chart,
            start_playback,
            stop_playback,
            playback_status,
            get_chart,
            update_steps,
            get_settings,
            set_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

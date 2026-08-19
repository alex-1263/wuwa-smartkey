//! M1 命令行播放器：完整调度（起手/循环/独立轨/free_fire）+ 全局热键启停。
//!
//! 用法：
//!   play <chart.json> [--dry-run] [--loops N] [--mode full|startup|loop] [--autostart]
//!
//! 热键：F6 开始（3 秒倒计时） / F7 停止。Ctrl+C 退出（建议用 F7 停止播放）。
//! --autostart 跳过热键直接播放（配合 --dry-run 做自动化验证）。

use std::sync::mpsc;
use std::time::Duration;

use engine::chart::ComboChart;
use engine::hotkey::{HotkeyListener, MOD_NOREPEAT, VK_F6, VK_F7};
use engine::scheduler::{Playback, PlaybackEvent, PlaybackMode, PlaybackOptions};
use engine::store;

const HK_START: i32 = 1;
const HK_STOP: i32 = 2;

enum Msg {
    Hotkey(i32),
    Ev(PlaybackEvent),
    Autostart,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let autostart = args.iter().any(|a| a == "--autostart");
    let loops = args
        .iter()
        .position(|a| a == "--loops")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u32>().ok());
    let mode = if args.iter().any(|a| a == "--mode" && a.contains("startup")) {
        PlaybackMode::StartupOnly
    } else if args.iter().any(|a| a == "--mode" && a.contains("loop")) {
        PlaybackMode::LoopOnly
    } else {
        PlaybackMode::Full
    };
    let path = match args.iter().find(|a| !a.starts_with("--")) {
        Some(p) => p.clone(),
        None => {
            eprintln!("用法: play <chart.json> [--dry-run] [--loops N] [--mode full|startup|loop]");
            std::process::exit(2);
        }
    };

    let chart: ComboChart = match std::fs::read_to_string(&path).ok() {
        Some(text) => ComboChart::parse(&text).unwrap_or_else(|e| {
            eprintln!("解析轴 JSON 失败: {e}");
            std::process::exit(1);
        }),
        // 不是文件路径时，尝试按轴库文件名加载
        None => store::load_chart(&path).unwrap_or_else(|e| {
            eprintln!("读取轴失败（路径或轴库中均未找到）: {e}");
            std::process::exit(1);
        }),
    };

    println!("已加载轴: {}  (起手 {} 步 / 循环 {} 步)",
        chart.title,
        chart.startup_steps().len(),
        chart.loop_steps().len(),
    );
    println!("待命：F6 开始 · F7 停止 · Ctrl+C 退出{}", if dry_run { "（干跑模式，不实际发键）" } else { "" });

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_hotkey = tx.clone();
    let _hotkeys = HotkeyListener::spawn(
        vec![
            (HK_START, MOD_NOREPEAT, VK_F6),
            (HK_STOP, MOD_NOREPEAT, VK_F7),
        ],
        Box::new(move |id| {
            let _ = tx_hotkey.send(Msg::Hotkey(id));
        }),
    );

    if autostart {
        let _ = tx.send(Msg::Autostart);
    }

    let mut playing: Option<Playback> = None;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Autostart | Msg::Hotkey(HK_START) if playing.is_none() => {
                if !dry_run && matches!(msg, Msg::Hotkey(_)) {
                    println!("3 秒倒计时，切到游戏窗口…");
                    for i in (1..=3).rev() {
                        println!("  {i}");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                let tx_ev = tx.clone();
                let opts = PlaybackOptions {
                    mode,
                    max_loops: loops,
                    dry_run,
                    ..Default::default()
                };
                playing = Some(Playback::spawn(chart.clone(), opts, move |ev| {
                    let _ = tx_ev.send(Msg::Ev(ev));
                }));
            }
            Msg::Hotkey(id) if id == HK_STOP => {
                if let Some(mut p) = playing.take() {
                    p.stop();
                }
            }
            Msg::Ev(ev) => {
                print_event(&ev);
                if matches!(ev, PlaybackEvent::Stopped { .. }) {
                    playing = None;
                    if autostart {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

fn print_event(ev: &PlaybackEvent) {
    match ev {
        PlaybackEvent::Started { title } => println!("▶ 开始: {title}"),
        PlaybackEvent::LoopRound { round } => println!("-- 循环第 {round} 轮 --"),
        PlaybackEvent::Switch { from, to } => {
            println!("  切人 {:?} → {to}", from.unwrap_or(0))
        }
        PlaybackEvent::FreeFire { wait_ms } => {
            println!("  ○ 自由发挥段，等待 {wait_ms}ms")
        }
        PlaybackEvent::StepDone {
            label,
            planned_ms,
            actual_ms,
            held_ms,
            ..
        } => println!(
            "  [{planned_ms:>6}ms/{actual_ms:>6}ms] {label}（按住 {held_ms}ms）"
        ),
        PlaybackEvent::StepSkipped { label, move_id } => {
            println!("  跳过 {label}（moveId={move_id} 无映射）")
        }
        PlaybackEvent::Stopped { reason } => match *reason {
            "manual" => println!("■ 已停止（手动）"),
            _ => println!("■ 播放完成"),
        },
    }
}

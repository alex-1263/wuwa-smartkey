//! M0 验证原型：读取 wwcombo 轴 JSON，执行起手轴前 N 步。
//!
//! 用法：
//!   m0 <chart.json> [--dry-run] [--steps N] [--loop]
//!   --dry-run  只打印执行计划，不发键
//!   --steps    限制步数（默认 3）
//!   --loop     执行循环轴而非起手轴

use std::time::{Duration, Instant};

use engine::chart::{ComboChart, ComboStep};
use engine::input::{self, Device};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let use_loop = args.iter().any(|a| a == "--loop");
    let step_limit: usize = args
        .iter()
        .position(|a| a == "--steps")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let path = match args.iter().find(|a| !a.starts_with("--")) {
        Some(p) => p.clone(),
        None => {
            eprintln!("用法: m0 <chart.json> [--dry-run] [--steps N] [--loop]");
            std::process::exit(2);
        }
    };

    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("读取轴文件失败 {path}: {e}");
        std::process::exit(1);
    });
    let chart: ComboChart = serde_json::from_str(&text).unwrap_or_else(|e| {
        eprintln!("解析轴 JSON 失败: {e}");
        std::process::exit(1);
    });

    let steps: Vec<&ComboStep> = if use_loop {
        chart.loop_steps()
    } else {
        chart.startup_steps()
    };
    if steps.is_empty() {
        eprintln!("轴中没有可执行步骤: {}", chart.title);
        std::process::exit(1);
    }
    let plan: Vec<&&ComboStep> = steps.iter().take(step_limit).collect();

    println!("轴: {}  步骤: {}/{}", chart.title, plan.len(), steps.len());
    if dry_run {
        for s in &plan {
            let dev = input::default_binding(&s.move_id);
            println!(
                "[plan] +{:>5}ms  {:<8} slot={:?} dev={} hold={}ms",
                s.start_min,
                s.label,
                s.character_slot,
                match dev {
                    Some(Device::Key(sc)) => format!("key:0x{sc:02X}"),
                    Some(Device::MouseLeft) => "mouse:L".into(),
                    Some(Device::MouseRight) => "mouse:R".into(),
                    None => "无映射,跳过".into(),
                },
                step_hold_ms(s),
            );
        }
        return;
    }

    println!("3 秒倒计时，切到游戏窗口…（Ctrl+C 中止）");
    for i in (1..=3).rev() {
        println!("  {i}");
        std::thread::sleep(Duration::from_secs(1));
    }

    input::begin_high_resolution_timer();
    let t0 = Instant::now();
    let mut cur_slot: Option<u8> = None;

    for s in plan {
        // 角色切换：slot 变化时先发数字键，等待切换动画
        if let Some(slot) = s.character_slot {
            if cur_slot.is_some() && cur_slot != Some(slot) {
                let switch_dev = input::default_binding(&format!("switch_{slot}"));
                if let Some(dev) = switch_dev {
                    wait_until(t0, s.start_min);
                    input::press(dev, Duration::from_millis(input::DEFAULT_TAP_MS));
                    println!(
                        "  [+{}ms] 切人 → {slot}",
                        t0.elapsed().as_millis()
                    );
                    std::thread::sleep(Duration::from_millis(input::SWITCH_DELAY_MS));
                }
            }
            cur_slot = Some(slot);
        }

        wait_until(t0, s.start_min);
        let Some(dev) = input::default_binding(&s.move_id) else {
            println!("  [+{}ms] {} 无默认映射，跳过", t0.elapsed().as_millis(), s.label);
            continue;
        };
        input::press(dev, Duration::from_millis(step_hold_ms(s)));
        println!(
            "  [+{}ms] {} (计划 {}ms)",
            t0.elapsed().as_millis(),
            s.label,
            s.start_min
        );
    }

    input::end_high_resolution_timer();
    println!("M0 执行完毕。");
}

fn step_hold_ms(s: &ComboStep) -> u64 {
    if input::is_hold_move(&s.move_id) {
        s.duration_min.max(input::DEFAULT_TAP_MS as i64) as u64
    } else {
        input::DEFAULT_TAP_MS
    }
}

fn wait_until(t0: Instant, target_ms: i64) {
    let target = t0 + Duration::from_millis(target_ms.max(0) as u64);
    let now = Instant::now();
    if target > now {
        std::thread::sleep(target - now);
    }
}

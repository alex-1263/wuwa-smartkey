//! 完整播放调度器：起手轴 → 循环轴轮播，独立轨并行，free_fire 静默等待。
//!
//! 停止语义：任何等待/按压都可被停止信号打断；播放线程退出前强制抬起所有按键。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crate::chart::{ComboChart, ComboStep, PeriodKind};
use crate::input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackMode {
    /// 起手轴 → 循环轴轮播（默认）
    Full,
    StartupOnly,
    LoopOnly,
    /// 半自动：切人键玩家手动按，招式自动打（见 semi.rs）
    Semi,
}

#[derive(Debug, Clone)]
pub struct PlaybackOptions {
    pub mode: PlaybackMode,
    /// 循环轮数上限，None = 无限
    pub max_loops: Option<u32>,
    /// 时间缩放（预留，M1 固定 1.0）
    pub speed: f64,
    /// 切人后等待切换动画的时长 ms
    pub switch_delay_ms: u64,
    /// free_fire 段最长等待 ms
    pub free_fire_timeout_ms: u64,
    /// 干跑：不发键不等待，只产生事件流
    pub dry_run: bool,
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            mode: PlaybackMode::Full,
            max_loops: None,
            speed: 1.0,
            switch_delay_ms: input::SWITCH_DELAY_MS,
            free_fire_timeout_ms: 10_000,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum PlaybackEvent {
    Started {
        title: String,
    },
    LoopRound {
        round: u32,
    },
    Switch {
        from: Option<u8>,
        to: u8,
    },
    FreeFire {
        wait_ms: i64,
    },
    StepDone {
        label: String,
        move_id: String,
        planned_ms: i64,
        actual_ms: i128,
        held_ms: u64,
    },
    /// 招式无输入映射，跳过
    StepSkipped {
        label: String,
        move_id: String,
    },
    Stopped {
        reason: &'static str,
    },
    // —— 半自动模式事件 ——
    /// 等待玩家手动切人（按下对应槽位的数字键后开打下一段）
    WaitingSwitch {
        slot: Option<u8>,
        round: u32,
        index: usize,
        total: usize,
        steps: usize,
        approx_ms: i64,
    },
    /// 一个角色段结束（completed = 打完；switched = 被玩家切人打断）
    SegmentDone {
        slot: Option<u8>,
        reason: &'static str,
    },
    /// 玩家按了轴上后续不存在的角色键，忽略并继续等待
    KeyIgnored {
        got: u8,
        expected: Option<u8>,
    },
}

pub struct Playback {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Playback {
    /// 启动播放线程。on_event 在播放线程回调，调用方自行转发（channel/Tauri emit）
    pub fn spawn(
        chart: ComboChart,
        opts: PlaybackOptions,
        on_event: impl Fn(PlaybackEvent) + Send + Sync + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            input::begin_high_resolution_timer();
            run(&chart, &opts, stop_thread.clone(), &on_event);
            // 任何退出路径（完成/手动停止/独立轨中途退出）都强制抬键
            stop_thread.store(true, Ordering::SeqCst);
            input::release_all();
            input::end_high_resolution_timer();
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// 请求停止并等待播放线程退出（含强制抬键）
    pub fn stop(&mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.finish();
    }
}

fn run(
    chart: &ComboChart,
    opts: &PlaybackOptions,
    stop: Arc<AtomicBool>,
    cb: &(dyn Fn(PlaybackEvent) + Send + Sync),
) {
    cb(PlaybackEvent::Started {
        title: chart.title.clone(),
    });

    // 注：lane(main/independent) 是"是否占连段推进"的语义标志，所有步骤
    // 在同一条时间线上执行（wwcombo 同款语义），无并行轨道。

    let t0 = Instant::now();
    let mut cur_slot: Option<u8> = None;
    let mut manual_stop = false;
    // 轴自带显式切人步骤时信任轴编排，禁用自动切人（其 300ms 等待
    // 不在轴的时间预算内，双发会造成数百 ms 的累积偏差）
    let auto_switch = !chart
        .steps
        .iter()
        .any(|s| !s.is_skippable() && s.move_id.starts_with("switch_"));

    if opts.mode != PlaybackMode::LoopOnly {
        if !exec_segment(
            chart,
            chart.startup_steps(),
            0.0,
            opts,
            &stop,
            cb,
            t0,
            &mut cur_slot,
            auto_switch,
        ) {
            manual_stop = true;
        }
    }

    if !manual_stop && opts.mode != PlaybackMode::StartupOnly {
        let t_len = loop_len(chart);
        // 干跑只演示 2 轮，避免无限输出
        let max_rounds = if opts.dry_run {
            opts.max_loops.map_or(2, |n| n.min(2))
        } else {
            opts.max_loops.unwrap_or(u32::MAX)
        };
        let mut round: u32 = 0;
        loop {
            cb(PlaybackEvent::LoopRound { round: round + 1 });
            let base = round as f64 * t_len;
            if !exec_segment(
                chart,
                chart.loop_steps(),
                base,
                opts,
                &stop,
                cb,
                t0,
                &mut cur_slot,
                auto_switch,
            ) {
                manual_stop = true;
                break;
            }
            round += 1;
            if round >= max_rounds {
                break;
            }
        }
    }

    cb(PlaybackEvent::Stopped {
        reason: if manual_stop { "manual" } else { "completed" },
    });
}

/// 执行一个段（起手或循环的某轮）。返回 false = 收到停止信号
#[allow(clippy::too_many_arguments)]
fn exec_segment(
    chart: &ComboChart,
    steps: Vec<&ComboStep>,
    base: f64,
    opts: &PlaybackOptions,
    stop: &AtomicBool,
    cb: &(dyn Fn(PlaybackEvent) + Send + Sync),
    t0: Instant,
    cur_slot: &mut Option<u8>,
    auto_switch: bool,
) -> bool {
    // free_fire 窗口映射到本段时间轴（循环轮内平移 base）
    let windows: Vec<(f64, f64)> = chart
        .periods
        .iter()
        .filter(|p| p.kind == PeriodKind::FreeFire)
        .map(|f| (base + f.start_ms, base + f.end_ms))
        .filter(|(s, e)| e > s)
        .collect();
    let in_free = |s: &ComboStep| {
        let at = base + s.start_min;
        windows.iter().any(|(fs, fe)| at >= *fs && at < *fe)
    };

    let wait_at = |t: f64| -> bool {
        if opts.dry_run {
            true
        } else {
            input::wait_until_interruptible(t0, t.round() as i64, stop)
        }
    };
    let sleep_ms = |ms: u64| -> bool {
        if opts.dry_run {
            true
        } else {
            input::sleep_interruptible(ms, stop)
        }
    };

    let mut wi = 0usize;
    for s in steps {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        // 自由发挥段内的步骤由玩家手动完成，播放器跳过
        if in_free(s) {
            continue;
        }
        let at = base + s.start_min;

        // 该步骤之前的 free_fire 窗口：到达窗口起点后静默等待
        while wi < windows.len() && windows[wi].0 <= at {
            let (fs, fe) = windows[wi];
            if !wait_at(fs) {
                return false;
            }
            let wait = (fe - fs).min(opts.free_fire_timeout_ms as f64);
            cb(PlaybackEvent::FreeFire {
                wait_ms: wait.round() as i64,
            });
            if !sleep_ms(wait.round() as u64) {
                return false;
            }
            wi += 1;
        }

        if !wait_at(at) {
            return false;
        }

        // 角色切换
        if let Some(slot) = s.character_slot {
            if auto_switch && *cur_slot != Some(slot) {
                let from = *cur_slot;
                if let Some(dev) = input::default_binding(&format!("switch_{slot}")) {
                    cb(PlaybackEvent::Switch { from, to: slot });
                    if !opts.dry_run {
                        input::press_interruptible(dev, input::DEFAULT_TAP_MS, stop);
                        sleep_ms(opts.switch_delay_ms);
                    }
                }
                *cur_slot = Some(slot);
            }
        }

        let Some(dev) = input::default_binding(&s.move_id) else {
            cb(PlaybackEvent::StepSkipped {
                label: s.label.clone(),
                move_id: s.move_id.clone(),
            });
            continue;
        };
        // 预输入：提前 preheatMs 按下（动作结束瞬间输入已被缓冲，衔接无缝）
        let preheat = s.preheat_ms.unwrap_or(0.0);
        if preheat > 0.0 && !wait_at(at - preheat) {
            return false;
        }
        let held = hold_ms(s);
        if opts.dry_run {
            cb(PlaybackEvent::StepDone {
                label: s.label.clone(),
                move_id: s.move_id.clone(),
                planned_ms: at.round() as i64,
                actual_ms: at.round() as i128,
                held_ms: held,
            });
        } else {
            input::press_interruptible(dev, held, stop);
            cb(PlaybackEvent::StepDone {
                label: s.label.clone(),
                move_id: s.move_id.clone(),
                planned_ms: at.round() as i64,
                actual_ms: t0.elapsed().as_millis() as i128,
                held_ms: held,
            });
        }
    }
    true
}

/// 按住时长（wwcombo practice.ts 语义）：
/// - 按住类（heavy_attack / *_hold）：必须按住 durationMin（游戏需要持续按住）
/// - 普通招式（普攻/技能/切人等）：短按点按立即完成，不等待 duration；
///   预输入由"提前到 startMin - preheatMs 按下"实现（见 exec_segment）
pub fn hold_ms(s: &ComboStep) -> u64 {
    if input::is_hold_move(&s.move_id) {
        s.duration_min.round().max(input::DEFAULT_TAP_MS as f64) as u64
    } else {
        input::DEFAULT_TAP_MS
    }
}

/// 循环周期长度：优先 loop_axis period，退化用最后一步的结束时间
fn loop_len(chart: &ComboChart) -> f64 {
    if let Some(p) = chart.period(PeriodKind::LoopAxis) {
        (p.end_ms - p.start_ms).max(1.0)
    } else {
        chart
            .steps
            .iter()
            .filter(|s| !s.is_skippable())
            .map(|s| s.start_min + s.duration_min)
            .fold(0.0_f64, |m, v| m.max(v))
            .max(1.0)
    }
}

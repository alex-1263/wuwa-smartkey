//! 半自动模式：切人键由玩家手动按（数字键 1-4），招式由播放器自动打。
//!
//! 原理：把轴按角色切成段；全局键盘钩子只听真人的数字键——
//! 玩家的切人按键就是"人肉同步信号"：
//! - 等待阶段：按下的槽位匹配下一段 → 从按键瞬间开打该段（段内相对时间）
//! - 播段阶段：再次按下数字键 → 立即抬键中断当前段，跳到匹配段
//! 开环播放的失步问题（被打断、预输入被切人覆盖）由此闭环：
//! 玩家的切人节奏就是轴的节奏，无需任何视觉/状态检测。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::chart::{ComboChart, ComboStep, PeriodKind};
use crate::input;
use crate::listener::{self, KeyListener};
use crate::scheduler::{hold_ms, PlaybackEvent, PlaybackOptions};

/// 一个角色段：切人步骤（或槽位变化）之间的一段连续招式
#[derive(Debug, Clone)]
pub struct Segment {
    /// 段属角色槽位（等待玩家按对应数字键触发）；None = 不挑键，任意数字键匹配
    pub slot: Option<u8>,
    /// 段在其 period 时间轴上的起点 ms（free_fire 窗口判断用）
    pub start_ms: f64,
    /// 段内步骤与相对时间（相对段起点 ms）
    pub steps: Vec<(ComboStep, f64)>,
    /// 段时长估计（最后一步结束 - 段起点）
    pub approx_ms: f64,
}

impl Segment {
    /// 该段是否由槽位键 slot 触发（无槽位信息的段接受任意键）
    fn matches(&self, slot: u8) -> bool {
        self.slot.map_or(true, |s| s == slot)
    }
}

/// 把一段时间线上的步骤切成角色段。
/// 段边界：显式切人步骤（switch_N），或 characterSlot 变化；
/// 切人步骤本身不进段（半自动下由玩家手动完成）；
/// 无槽位信息的步骤跟随当前段。
pub fn segment_steps(steps: &[&ComboStep]) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    // 当前已开段的槽位（外层 Option 区分"还没开段"与"段槽位未知"）
    let mut cur: Option<Option<u8>> = None;
    for s in steps {
        let is_switch = s.move_id.starts_with("switch_");
        let slot_changed = match (cur.flatten(), s.character_slot) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        if is_switch || slot_changed || segs.is_empty() {
            let slot = s
                .character_slot
                .or_else(|| switch_slot_from_move_id(&s.move_id));
            segs.push(Segment {
                slot,
                start_ms: s.start_min,
                steps: Vec::new(),
                approx_ms: 0.0,
            });
            cur = Some(slot);
            if is_switch {
                continue;
            }
        }
        let seg = segs.last_mut().unwrap();
        seg.steps.push(((*s).clone(), s.start_min - seg.start_ms));
        let end = s.start_min + s.duration_min.max(0.0);
        seg.approx_ms = seg.approx_ms.max(end - seg.start_ms);
    }
    segs
}

/// "switch_2" → 2
fn switch_slot_from_move_id(id: &str) -> Option<u8> {
    id.strip_prefix("switch_")?
        .parse()
        .ok()
        .filter(|n| (1..=4).contains(n))
}

/// 在 queue[from..] 中找第一个匹配槽位的段下标
fn find_from(queue: &[&Segment], from: usize, slot: u8) -> Option<usize> {
    queue[from..]
        .iter()
        .position(|s| s.matches(slot))
        .map(|p| p + from)
}

enum KeyEv {
    /// 到点
    Ok,
    Stopped,
    Switch(u8),
}

/// 等待切人键（无截止时间）。非数字键直接丢弃
fn wait_key(stop: &AtomicBool, rx: &Receiver<u32>) -> KeyEv {
    loop {
        if stop.load(Ordering::SeqCst) {
            return KeyEv::Stopped;
        }
        while let Ok(vk) = rx.try_recv() {
            if let Some(slot) = listener::slot_of(vk) {
                return KeyEv::Switch(slot);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 等到 t0 + target_ms；期间真数字键/停止信号可提前返回
fn wait_until_key(t0: Instant, target_ms: f64, stop: &AtomicBool, rx: &Receiver<u32>) -> KeyEv {
    let deadline = t0 + Duration::from_millis(target_ms.max(0.0) as u64);
    loop {
        if stop.load(Ordering::SeqCst) {
            return KeyEv::Stopped;
        }
        while let Ok(vk) = rx.try_recv() {
            if let Some(slot) = listener::slot_of(vk) {
                return KeyEv::Switch(slot);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return KeyEv::Ok;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
    }
}

enum SegEnd {
    Completed,
    /// 段中被玩家切人键打断，携带按下的槽位
    Switched(u8),
    Stopped,
}

/// 播放一个段：t0 = 触发按键瞬间，步骤按相对时间执行。
/// 不发切人键（切段时已移除）；free_fire 窗口内步骤跳过（玩家手动完成）。
#[allow(clippy::too_many_arguments)]
fn play_segment(
    chart: &ComboChart,
    seg: &Segment,
    opts: &PlaybackOptions,
    stop: &AtomicBool,
    rx: &Receiver<u32>,
    cb: &(dyn Fn(PlaybackEvent) + Send + Sync),
) -> SegEnd {
    let t0 = Instant::now();
    let frees: Vec<(f64, f64)> = chart
        .periods
        .iter()
        .filter(|p| p.kind == PeriodKind::FreeFire)
        .map(|f| (f.start_ms, f.end_ms))
        .collect();
    let in_free = |abs: f64| frees.iter().any(|(a, b)| abs >= *a && abs < *b && *b > *a);

    for (s, rel) in &seg.steps {
        if in_free(seg.start_ms + rel) {
            continue;
        }
        // 预输入：提前 preheat 按下
        let press_at = rel - s.preheat_ms.unwrap_or(0.0);
        match wait_until_key(t0, press_at, stop, rx) {
            KeyEv::Ok => {}
            KeyEv::Stopped => return SegEnd::Stopped,
            KeyEv::Switch(slot) => return SegEnd::Switched(slot),
        }
        let Some(dev) = input::default_binding(&s.move_id) else {
            cb(PlaybackEvent::StepSkipped {
                label: s.label.clone(),
                move_id: s.move_id.clone(),
            });
            continue;
        };
        let held = hold_ms(s);
        if opts.dry_run {
            cb(PlaybackEvent::StepDone {
                label: s.label.clone(),
                move_id: s.move_id.clone(),
                planned_ms: rel.round() as i64,
                actual_ms: rel.round() as i128,
                held_ms: held,
            });
        } else {
            // 按住期间也可被切人键打断；无论结果如何都保证 up 配对
            input::down_tracked(dev);
            let end = wait_until_key(t0, press_at + held as f64, stop, rx);
            input::up_tracked(dev);
            cb(PlaybackEvent::StepDone {
                label: s.label.clone(),
                move_id: s.move_id.clone(),
                planned_ms: rel.round() as i64,
                actual_ms: t0.elapsed().as_millis() as i128,
                held_ms: held,
            });
            match end {
                KeyEv::Ok => {}
                KeyEv::Stopped => return SegEnd::Stopped,
                KeyEv::Switch(slot) => return SegEnd::Switched(slot),
            }
        }
    }
    SegEnd::Completed
}

fn run_semi(
    chart: &ComboChart,
    opts: &PlaybackOptions,
    stop: Arc<AtomicBool>,
    rx: &Receiver<u32>,
    cb: &(dyn Fn(PlaybackEvent) + Send + Sync),
) {
    cb(PlaybackEvent::Started {
        title: chart.title.clone(),
    });

    let startup = segment_steps(&chart.startup_steps());
    let loopsegs = segment_steps(&chart.loop_steps());
    if startup.is_empty() && loopsegs.is_empty() {
        cb(PlaybackEvent::Stopped {
            reason: "completed",
        });
        return;
    }

    // 第 1 轮 = 起手段 + 循环段，其后每轮 = 循环段
    let mut cur_slot: Option<u8> = None;
    let mut manual_stop = false;
    let mut round: u32 = 0;
    // 起手轮算 1 轮，max_loops 限制的是其后的循环轮数
    let max_rounds = 1 + opts.max_loops.unwrap_or(u32::MAX);

    'outer: while round < max_rounds {
        let queue: Vec<&Segment> = if round == 0 {
            startup.iter().chain(loopsegs.iter()).collect()
        } else {
            loopsegs.iter().collect()
        };
        if queue.is_empty() {
            break;
        }
        let mut i = 0usize;
        while i < queue.len() {
            // —— 等待阶段：玩家按匹配键 ——
            loop {
                let seg = queue[i];
                cb(PlaybackEvent::WaitingSwitch {
                    slot: seg.slot,
                    round: round + 1,
                    index: i + 1,
                    total: queue.len(),
                    steps: seg.steps.len(),
                    approx_ms: seg.approx_ms.round() as i64,
                });
                let ev = if opts.dry_run {
                    // 干跑无人按键：自动按期望键推进（段槽位未知时按 1）
                    KeyEv::Switch(seg.slot.unwrap_or(1))
                } else {
                    wait_key(&stop, rx)
                };
                match ev {
                    KeyEv::Stopped => {
                        manual_stop = true;
                        break 'outer;
                    }
                    KeyEv::Switch(slot) => {
                        if !seg.matches(slot) {
                            // 轴上后续存在的角色键：跳段（玩家手动同步节奏）
                            match find_from(&queue, i + 1, slot) {
                                Some(j) => i = j,
                                None => {
                                    cb(PlaybackEvent::KeyIgnored {
                                        got: slot,
                                        expected: seg.slot,
                                    });
                                    continue;
                                }
                            }
                        }
                        break;
                    }
                    KeyEv::Ok => unreachable!("wait_key 无截止时间"),
                }
            }

            // —— 播段阶段 ——
            let seg = queue[i];
            cb(PlaybackEvent::Switch {
                from: cur_slot,
                to: seg.slot.unwrap_or(0),
            });
            cur_slot = seg.slot;
            loop {
                match play_segment(chart, queue[i], opts, &stop, rx, cb) {
                    SegEnd::Completed => {
                        cb(PlaybackEvent::SegmentDone {
                            slot: queue[i].slot,
                            reason: "completed",
                        });
                        i += 1;
                        break;
                    }
                    SegEnd::Switched(slot) => {
                        cb(PlaybackEvent::SegmentDone {
                            slot: queue[i].slot,
                            reason: "switched",
                        });
                        // 从当前段（含）向后找：重按当前槽位 = 重打本段
                        match find_from(&queue, i, slot) {
                            Some(j) if j == i => continue,
                            Some(j) => {
                                i = j;
                                break;
                            }
                            // 后续没有该角色：回到等待阶段（重等当前段）
                            None => break,
                        }
                    }
                    SegEnd::Stopped => {
                        manual_stop = true;
                        break 'outer;
                    }
                }
            }
        }
        round += 1;
    }

    cb(PlaybackEvent::Stopped {
        reason: if manual_stop { "manual" } else { "completed" },
    });
}

/// 半自动播放器：spawn 时挂全局键盘钩子，drop/stop 时先停播放线程再卸钩子
pub struct SemiPlayback {
    stop: Arc<AtomicBool>,
    listener: Option<KeyListener>,
    handle: Option<JoinHandle<()>>,
}

impl SemiPlayback {
    pub fn spawn(
        chart: ComboChart,
        opts: PlaybackOptions,
        on_event: impl Fn(PlaybackEvent) + Send + Sync + 'static,
    ) -> Self {
        let (listener, rx) = KeyListener::spawn();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            input::begin_high_resolution_timer();
            run_semi(&chart, &opts, stop_thread.clone(), &rx, &on_event);
            stop_thread.store(true, Ordering::SeqCst);
            input::release_all();
            input::end_high_resolution_timer();
        });
        Self {
            stop,
            listener: Some(listener),
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        if let Some(mut l) = self.listener.take() {
            l.stop();
        }
    }
}

impl Drop for SemiPlayback {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, move_id: &str, slot: Option<u8>, start: f64) -> ComboStep {
        ComboStep {
            id: id.into(),
            move_id: move_id.into(),
            label: move_id.into(),
            character_slot: slot,
            start_min: start,
            duration_min: 100.0,
            ..Default::default()
        }
    }

    #[test]
    fn segments_split_by_switch_and_slot_change() {
        let steps = vec![
            step("a", "basic_attack", Some(1), 0.0),
            step("b", "resonance_skill", Some(1), 500.0),
            step("c", "switch_2", Some(2), 1000.0), // 显式切人 → 新段（切人步骤不进段）
            step("d", "basic_attack", Some(2), 1200.0),
            step("e", "heavy_attack", Some(2), 1500.0),
            step("f", "basic_attack", Some(3), 2000.0), // 槽位变化 → 新段
            step("g", "echo_skill", None, 2200.0),      // 无槽位 → 跟随段 3
        ];
        let refs: Vec<&ComboStep> = steps.iter().collect();
        let segs = segment_steps(&refs);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].slot, Some(1));
        assert_eq!(segs[0].steps.len(), 2);
        assert_eq!(segs[0].steps[0].1, 0.0);
        assert_eq!(segs[0].approx_ms, 600.0);

        assert_eq!(segs[1].slot, Some(2));
        // switch 步骤不进段：段内第一个是 d，rel = 1200 - 1000
        assert_eq!(segs[1].steps.len(), 2);
        assert_eq!(segs[1].steps[0].0.id, "d");
        assert_eq!(segs[1].steps[0].1, 200.0);

        assert_eq!(segs[2].slot, Some(3));
        assert_eq!(segs[2].steps.len(), 2);
        assert_eq!(segs[2].steps[1].0.id, "g");
    }

    #[test]
    fn none_slot_follows_and_matches_any() {
        let steps = vec![
            step("a", "basic_attack", None, 0.0),
            step("b", "resonance_skill", None, 300.0),
        ];
        let refs: Vec<&ComboStep> = steps.iter().collect();
        let segs = segment_steps(&refs);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].slot, None);
        assert_eq!(segs[0].steps.len(), 2);
        assert!(segs[0].matches(1) && segs[0].matches(4));
    }

    #[test]
    fn switch_move_id_slot_parsed() {
        assert_eq!(switch_slot_from_move_id("switch_3"), Some(3));
        assert_eq!(switch_slot_from_move_id("switch_9"), None);
        assert_eq!(switch_slot_from_move_id("basic_attack"), None);
    }

    #[test]
    fn find_from_skips_non_matching() {
        let steps = vec![
            step("a", "basic_attack", Some(1), 0.0),
            step("b", "switch_2", Some(2), 100.0),
            step("c", "basic_attack", Some(2), 200.0),
            step("d", "switch_3", Some(3), 300.0),
            step("e", "basic_attack", Some(3), 400.0),
        ];
        let refs: Vec<&ComboStep> = steps.iter().collect();
        let segs = segment_steps(&refs);
        let queue: Vec<&Segment> = segs.iter().collect();
        assert_eq!(find_from(&queue, 0, 2), Some(1));
        assert_eq!(find_from(&queue, 2, 3), Some(2));
        assert_eq!(find_from(&queue, 1, 4), None);
    }
}

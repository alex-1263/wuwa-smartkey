//! wwcombo ComboChart 数据模型。
//! 字段名与 wwcombo `combo-core/types.ts` 保持一致（camelCase），
//! 解析对未知字段宽容，以兼容其格式演进。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboChart {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub character: Option<String>,
    #[serde(default)]
    pub character_count: Option<u8>,
    #[serde(default)]
    pub steps: Vec<ComboStep>,
    #[serde(default)]
    pub periods: Vec<ComboPeriod>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboStep {
    pub id: String,
    pub move_id: String,
    pub label: String,
    #[serde(default)]
    pub character_slot: Option<u8>,
    #[serde(default)]
    pub lane: Lane,
    #[serde(default)]
    pub start_min: i64,
    #[serde(default)]
    pub start_max: i64,
    #[serde(default)]
    pub duration_min: i64,
    #[serde(default)]
    pub duration_max: i64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    #[default]
    Main,
    Independent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    DraftPeriod,
    FreeFire,
    StartupAxis,
    LoopAxis,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboPeriod {
    pub id: String,
    pub kind: PeriodKind,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub start_ms: i64,
    #[serde(default)]
    pub end_ms: i64,
    #[serde(default)]
    pub loop_index: Option<u32>,
}

impl ComboStep {
    /// 录制控制类与纯展示招式，播放器跳过
    pub fn is_skippable(&self) -> bool {
        matches!(
            self.move_id.as_str(),
            "start_challenge" | "stop_recording" | "empty_action"
        )
    }
}

impl ComboChart {
    /// 起手轴步骤（主轨、可执行、按开始时间排序）。
    /// 无 startup_axis period 时退化为全部主轨步骤。
    pub fn startup_steps(&self) -> Vec<&ComboStep> {
        self.steps_in_period(PeriodKind::StartupAxis)
    }

    pub fn loop_steps(&self) -> Vec<&ComboStep> {
        self.steps_in_period(PeriodKind::LoopAxis)
    }

    fn steps_in_period(&self, kind: PeriodKind) -> Vec<&ComboStep> {
        let range = self
            .periods
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| (p.start_ms, p.end_ms));
        let mut steps: Vec<&ComboStep> = self
            .steps
            .iter()
            .filter(|s| s.lane == Lane::Main && !s.is_skippable())
            .filter(|s| match range {
                Some((lo, hi)) => s.start_min >= lo && s.start_min < hi,
                None => true,
            })
            .collect();
        steps.sort_by_key(|s| s.start_min);
        steps
    }
}

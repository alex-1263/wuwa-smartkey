//! wwcombo ComboChart 数据模型。
//!
//! 以 wwcombo `combo-core/types.ts`（v3）为权威来源逐字段对齐重写，
//! 字段名 camelCase、可选性、语义与官方类型 1:1。
//! 解析对未知字段宽容（serde 默认忽略），以兼容官方格式迭代。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboChart {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub character_count: Option<u8>,
    #[serde(default)]
    pub character: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub community: Option<ComboCommunityMetadata>,
    #[serde(default)]
    pub content_labels: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub timeline_duration_ms: Option<f64>,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub start_trigger_move_id: Option<String>,
    #[serde(default)]
    pub stop_trigger_move_id: Option<String>,
    #[serde(default)]
    pub steps: Vec<ComboStep>,
    #[serde(default)]
    pub periods: Vec<ComboPeriod>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboStep {
    pub id: String,
    pub move_id: String,
    pub label: String,
    #[serde(default)]
    pub custom_label: Option<bool>,
    /// 角色槽位（1-4），非执行次数
    #[serde(default)]
    pub character_slot: Option<u8>,
    #[serde(default)]
    pub workshop_lane: Option<String>,
    /// 是否占连段推进的语义标志，不是分轨
    #[serde(default)]
    pub lane: Lane,
    #[serde(default)]
    pub independent: bool,
    #[serde(default)]
    pub start_min: f64,
    #[serde(default)]
    pub start_max: f64,
    #[serde(default)]
    pub duration_min: f64,
    #[serde(default)]
    pub duration_max: f64,
    /// 前摇（预输入）：输入提前到 startMin - preheatMs 按下
    #[serde(default)]
    pub preheat_ms: Option<f64>,
    #[serde(default)]
    pub recovery_ms: Option<f64>,
    #[serde(default)]
    pub manual_free: Option<bool>,
    #[serde(default)]
    pub free: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
    /// 招式块颜色（可视化用）
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub advances_step: bool,
    #[serde(default)]
    pub samples: Vec<ComboStepSample>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboStepSample {
    #[serde(default)]
    pub recording_id: String,
    #[serde(default)]
    pub start_time: f64,
    #[serde(default)]
    pub duration: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComboCommunityMetadata {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub characters: Vec<String>,
    #[serde(default)]
    pub rounds: u32,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub wheelchair_eligible: bool,
    #[serde(default)]
    pub exported_at: i64,
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
    pub character_slot: Option<u8>,
    #[serde(default)]
    pub lane: Option<Lane>,
    #[serde(default)]
    pub start_ms: f64,
    #[serde(default)]
    pub end_ms: f64,
    #[serde(default)]
    pub loop_index: Option<u32>,
}

/// 步骤时间补丁（编辑器保存用）：按 id 定位，start/duration 的 min/max 同步写
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepPatch {
    pub id: String,
    pub start_min: f64,
    pub duration_min: f64,
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
    /// 兼容两种文件布局：
    /// - 裸 ComboChart（自建/简易宏）
    /// - wwcombo 导出包装：{"type":"wwcombo-chart","version":3,"chart":{...}}
    pub fn parse(text: &str) -> Result<ComboChart, serde_json::Error> {
        let v: serde_json::Value = serde_json::from_str(text)?;
        match v.get("chart") {
            Some(inner) if inner.is_object() => serde_json::from_value(inner.clone()),
            _ => serde_json::from_value(v),
        }
    }

    pub fn period(&self, kind: PeriodKind) -> Option<&ComboPeriod> {
        self.periods.iter().find(|p| p.kind == kind)
    }

    /// 起手轴步骤（可执行、按开始时间排序）。
    /// 无 startup_axis period 时退化为全部可执行步骤。
    pub fn startup_steps(&self) -> Vec<&ComboStep> {
        self.steps_in_period(PeriodKind::StartupAxis)
    }

    pub fn loop_steps(&self) -> Vec<&ComboStep> {
        self.steps_in_period(PeriodKind::LoopAxis)
    }

    fn steps_in_period(&self, kind: PeriodKind) -> Vec<&ComboStep> {
        // lane 是"是否占连段推进"的语义标志（wwcombo 同款），不是分轨：
        // 所有步骤（main/independent）都在同一条时间线上参与排布与播放
        let range = self
            .periods
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| (p.start_ms, p.end_ms));
        let mut steps: Vec<&ComboStep> = self
            .steps
            .iter()
            .filter(|s| !s.is_skippable())
            .filter(|s| match range {
                Some((lo, hi)) => s.start_min >= lo && s.start_min < hi,
                None => true,
            })
            .collect();
        steps.sort_by(|a, b| a.start_min.total_cmp(&b.start_min));
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成 v3 包装格式 fixture，覆盖官方类型全部关键字段
    const FIXTURE: &str = r##"{
      "type": "wwcombo-chart",
      "version": 3,
      "chart": {
        "id": "test-1",
        "title": "测试轴",
        "characterCount": 3,
        "character": "测试角色",
        "author": "tester",
        "tags": ["全局"],
        "community": {
          "id": "test-1",
          "name": "测试轴",
          "tags": ["全局"],
          "description": "desc",
          "characters": ["A", "B"],
          "rounds": 5,
          "link": "https://example.com",
          "wheelchairEligible": true,
          "exportedAt": 1755000000000
        },
        "version": 1,
        "createdAt": 1755000000000,
        "updatedAt": 1755000000000,
        "startTriggerMoveId": "start_challenge",
        "stopTriggerMoveId": "stop_recording",
        "steps": [
          {
            "id": "s1",
            "moveId": "basic_attack",
            "label": "普攻",
            "characterSlot": 1,
            "lane": "main",
            "independent": false,
            "startMin": 0,
            "startMax": 0,
            "durationMin": 500,
            "durationMax": 500,
            "preheatMs": 80,
            "recoveryMs": 40,
            "manualFree": false,
            "free": false,
            "note": "预输入普攻",
            "color": "#7fd1ae",
            "advancesStep": false,
            "samples": []
          }
        ],
        "periods": [
          { "id": "p1", "kind": "startup_axis", "label": "起手", "startMs": 0, "endMs": 1200 }
        ]
      }
    }"##;

    #[test]
    fn parses_v3_wrapped_fixture() {
        let c = ComboChart::parse(FIXTURE).expect("解析 v3 包装格式失败");
        assert_eq!(c.id, "test-1");
        assert_eq!(c.title, "测试轴");
        assert_eq!(c.community.as_ref().unwrap().rounds, 5);
        assert_eq!(c.steps.len(), 1);
        let s = &c.steps[0];
        assert_eq!(s.move_id, "basic_attack");
        assert_eq!(s.character_slot, Some(1));
        assert_eq!(s.preheat_ms, Some(80.0));
        assert_eq!(s.recovery_ms, Some(40.0));
        assert_eq!(s.color.as_deref(), Some("#7fd1ae"));
        assert_eq!(c.period(PeriodKind::StartupAxis).unwrap().end_ms, 1200.0);
    }

    #[test]
    fn parses_bare_format() {
        let c = ComboChart::parse(r#"{"id":"x","title":"裸格式"}"#).expect("解析裸格式失败");
        assert_eq!(c.id, "x");
        assert!(c.steps.is_empty());
    }
}

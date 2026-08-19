//! 轴库存储：%APPDATA%/wuwa-smartkey/charts 下的 JSON 文件管理。
//! 一个轴一个文件，文件名 = chart.id（清洗后）.json。

use std::fs;
use std::path::{Path, PathBuf};

use crate::chart::ComboChart;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChartMeta {
    pub id: String,
    pub title: String,
    pub character: Option<String>,
    pub tags: Vec<String>,
    /// 轴库内的文件名（非完整路径）
    pub file: String,
}

pub fn charts_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = Path::new(&base).join("wuwa-smartkey").join("charts");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn list_charts() -> Vec<ChartMeta> {
    let Ok(dir) = charts_dir() else {
        return vec![];
    };
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "json") {
                continue;
            }
            let Some(file) = path.file_name() else { continue };
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(c) = ComboChart::parse(&text) {
                    out.push(ChartMeta {
                        id: c.id,
                        title: c.title,
                        character: c.character,
                        tags: c.tags,
                        file: file.to_string_lossy().into_owned(),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// 导入轴文件到轴库（校验可解析后落盘）
pub fn import_from(src: &Path) -> std::io::Result<ChartMeta> {
    let text = fs::read_to_string(src)?;
    let chart: ComboChart =
        ComboChart::parse(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let dir = charts_dir()?;
    let file = format!("{}.json", sanitize(&chart.id));
    fs::write(dir.join(&file), &text)?;
    Ok(ChartMeta {
        id: chart.id,
        title: chart.title,
        character: chart.character,
        tags: chart.tags,
        file,
    })
}

/// 按轴库文件名读取完整轴
pub fn load_chart(file: &str) -> std::io::Result<ComboChart> {
    let dir = charts_dir()?;
    let name = safe_name(file)?;
    let text = fs::read_to_string(dir.join(name))?;
    ComboChart::parse(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn delete(file: &str) -> std::io::Result<()> {
    let dir = charts_dir()?;
    fs::remove_file(dir.join(safe_name(file)?))
}

/// 防路径穿越：只取文件名部分
fn safe_name(file: &str) -> std::io::Result<String> {
    Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "非法文件名"))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 应用设置（热键等），持久化在 %APPDATA%/wuwa-smartkey/settings.json
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub hotkey_start: Option<String>,
    pub hotkey_stop: Option<String>,
    pub hotkey_restart: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey_start: None,
            hotkey_stop: None,
            hotkey_restart: None,
        }
    }
}

pub const DEFAULT_HOTKEYS: (&str, &str, &str) = ("F6", "F7", "F8");

/// 读取设置；未配置的热键回填默认值（F6/F7/F8）
pub fn load_settings() -> Settings {
    let path = match settings_path() {
        Ok(p) => p,
        Err(_) => return Settings::default(),
    };
    let parsed = fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        .unwrap_or_default();
    Settings {
        hotkey_start: Some(
            parsed
                .hotkey_start
                .unwrap_or_else(|| DEFAULT_HOTKEYS.0.into()),
        ),
        hotkey_stop: Some(
            parsed
                .hotkey_stop
                .unwrap_or_else(|| DEFAULT_HOTKEYS.1.into()),
        ),
        hotkey_restart: Some(
            parsed
                .hotkey_restart
                .unwrap_or_else(|| DEFAULT_HOTKEYS.2.into()),
        ),
    }
}

pub fn save_settings(s: &Settings) -> std::io::Result<()> {
    let path = settings_path()?;
    let text = serde_json::to_string_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text)
}

/// 无损更新步骤时间：读轴库文件为 JSON 树，仅替换匹配步骤的
/// startMin/startMax/durationMin/durationMax，其余字段（含 wwcombo
/// 包装层、community、bindings 等）原样保留写回。
pub fn patch_steps(file: &str, patches: &[crate::chart::StepPatch]) -> std::io::Result<usize> {
    use serde_json::json;
    let dir = charts_dir()?;
    let path = dir.join(safe_name(file)?);
    let text = fs::read_to_string(&path)?;
    let mut v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let steps = if v.get("chart").map(|c| c.is_object()).unwrap_or(false) {
        v["chart"]["steps"].as_array_mut()
    } else {
        v["steps"].as_array_mut()
    };
    let Some(steps) = steps else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "轴文件无 steps 数组"));
    };
    let mut applied = 0;
    for step in steps.iter_mut() {
        let Some(id) = step["id"].as_str() else { continue };
        let Some(p) = patches.iter().find(|p| p.id == id) else { continue };
        step["startMin"] = json!(p.start_min);
        step["startMax"] = json!(p.start_min);
        step["durationMin"] = json!(p.duration_min);
        step["durationMax"] = json!(p.duration_min);
        applied += 1;
    }
    let out = serde_json::to_string_pretty(&v)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&path, out)?;
    Ok(applied)
}

fn settings_path() -> std::io::Result<PathBuf> {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = Path::new(&base).join("wuwa-smartkey");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("settings.json"))
}

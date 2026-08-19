//! 轴库存储：%APPDATA%/wuwa-smartkey/charts 下的 JSON 文件管理。
//! 一个轴一个文件，文件名 = chart.id（清洗后）.json。

use std::fs;
use std::path::{Path, PathBuf};

use crate::chart::ComboChart;

#[derive(Debug, Clone)]
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
                if let Ok(c) = serde_json::from_str::<ComboChart>(&text) {
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
    let chart: ComboChart = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
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
    serde_json::from_str(&text)
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

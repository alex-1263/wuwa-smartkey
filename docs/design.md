# wuwa-smartkey 设计文档

> 鸣潮按键宏工具 · 轴播放器 · 开工文档
>
> 状态：v1.0（2026-08-19 定稿）

---

## 1. 项目定位

一个面向《鸣潮》的**按键宏工具**（按键精灵思路）：

- **底座是通用自动按键引擎**：按键、长按、延时、循环、热键启停；
- **数据来源开放**：支持导入 [wwcombo](https://github.com/NovaWallace/wwcombo) 的连段轴（ComboChart JSON）、用户自己录制、手动自定义；
- **向上做生态**：内部格式与 wwcombo 兼容，社区轴可以在 wwcombo 里练习、在本工具中执行。

**不是什么**：不做视觉识别自动化（那是 ok-ww 的领域）；不做后台窗口发键（Unity 游戏不可行）；V1 不做在线轴库。

### 1.1 用户使用旅程

1. 打开软件 → 导入轴（wwcombo JSON / 录制 / 简易宏表单生成）→ 看到轴概览
2. 键位映射自动加载（优先读 wwcombo 的 `.wwkeys.json`，否则用游戏默认表）
3. 进游戏站好位
4. 按全局热键（默认 F6）→ 3 秒倒计时 → 开始执行（起手轴 → 循环轴轮播）
5. 执行中悬浮提示进度（当前步骤、循环轮次）
6. 任何时刻按停止热键（默认 F7）→ 立即停止，所有按住的键强制抬起

---

## 2. 参考项目

| 项目 | 定位 | 对本项目的价值 |
|---|---|---|
| [ok-wuthering-waves](https://github.com/ok-oldking/ok-wuthering-waves) | 视觉识别全自动战斗（Python + YOLO + OCR） | 证明用户态 `SendInput` + 扫描码可对鸣潮生效；发键/时序实现可对照 |
| [wwcombo](https://github.com/NovaWallace/wwcombo) | 连段轴的录制/编辑/练习工具（Tauri 2 + TS），只读输入不发键 | **轴数据格式的权威来源**（`combo-core/types.ts`、`defaults.ts`）；前端轴渲染可借鉴 |

---

## 3. 技术栈（定稿）

| 层 | 选型 | 说明 |
|---|---|---|
| 桌面壳 | **Tauri 2** | 系统 WebView2（Win10 1809+/Win11 自带），产物 1~3MB；与 wwcombo 同栈 |
| 后端 | **Rust** | serde 解析轴数据；`windows-rs`（微软官方）调 SendInput |
| 前端 | **Vue 3 + Vite + Naive UI** | 轴管理、播放控制、可视化 |
| 关键 crate | `serde` / `serde_json`、`windows`、`rdev`（录制钩子） | 热键用 `tauri-plugin-global-shortcut` |
| 打包 | 嵌入 `requireAdministrator` manifest | 否则游戏提权运行时输入被 UIPI 拦截 |

兼容性边界：依赖 WebView2 Runtime，不支持 Win7/8 —— 与鸣潮本身的系统要求重合，无实际影响。

---

## 4. 总体架构

核心决策：**内部统一格式 = wwcombo 的 ComboChart（超集）**。三种数据来源全部归一到它，播放引擎只有一份。

```
wwcombo JSON ──导入(直通)──┐
                           ├──→ 统一 ComboChart ──→ 播放引擎（唯一）
录制事件流 ──识别/转换────┤                └──→ 分享导出（剥离私有字段 = wwcombo 格式）
简易宏表单 ──生成──────────┘
```

```
wuwa-smartkey/
├─ docs/                      # 本文档
├─ src-tauri/
│  ├─ src/
│  │  ├─ chart/               # serde 模型 + 加载/保存/导出
│  │  ├─ scheduler/           # 播放线程：指令 channel + AtomicBool 停止
│  │  ├─ input/               # SendInput 封装（键盘扫描码+鼠标）、键位映射
│  │  ├─ recorder/            # 全局键鼠钩子 + 事件→招式识别
│  │  └─ commands.rs          # 暴露给前端的 Tauri command
│  └─ tauri.conf.json
└─ src/                       # Vue 前端
   ├─ 轴库管理 / 播放控制 / 日志
   ├─ 简易宏编辑器（表单式）
   └─ 轴可视化（P1）
```

`chart/`、`scheduler/`、`input/`、`recorder/` 保持纯净（不依赖 Tauri），可用独立 bin 测试；UI 只是薄壳。

---

## 5. 数据模型

### 5.1 内部格式 = ComboChart 超集

以 wwcombo `combo-core/types.ts` 为准（字段名不变，解析对未知字段宽容以兼容其版本迭代），新增私有字段挂在独立命名空间（如 `smartkey` 字段内），导出分享时剥离。

```rust
// 核心结构示意（serde 反序列化）
struct ComboChart {
    id: String,
    title: String,
    character: Option<String>,
    author: Option<String>,
    tags: Vec<String>,
    character_count: Option<u8>,      // 3 | 4
    version: u32,
    steps: Vec<ComboStep>,            // 招式步骤
    periods: Option<Vec<ComboPeriod>>, // 时间段（起手/循环/自由）
    // + 私有字段：smartkey: { playback: PlaybackConfig }
}

struct ComboStep {
    id: String,
    move_id: String,                  // 关联招式，见 5.2 映射表
    label: String,
    character_slot: Option<u8>,       // 1~4，由哪个角色执行
    lane: Lane,                       // main（主轨顺序）/ independent（独立轨并行）
    start_min: i64, start_max: i64,   // 相对轴起点的最早/最晚开始（ms）
    duration_min: i64, duration_max: i64, // 按压时长区间（ms）
    preheat_ms: Option<i64>,          // 前摇
    recovery_ms: Option<i64>,         // 后摇
    free: Option<bool>,               // 自由发挥段标记
    note: Option<String>,
    // ...
}

struct ComboPeriod {
    kind: PeriodKind,   // startup_axis | loop_axis | free_fire | draft_period
    start_ms: i64,
    end_ms: i64,
    loop_index: Option<u32>,
    // ...
}
```

### 5.2 招式 → 输入映射（默认表，来自 wwcombo `defaults.ts`）

| moveId | 招式 | 默认键鼠输入 |
|---|---|---|
| `basic_attack` | 普攻 | 鼠标左键 |
| `heavy_attack` | 重击 | 鼠标左键**长按** |
| `skill` / `skill_hold` | 技能 / 长按技能 | E / E 长按 |
| `echo` / `echo_hold` | 声骸 / 长按声骸 | Q / Q 长按 |
| `liberation` / `liberation_hold` | 共鸣解放 / 长按 | R / R 长按 |
| `dodge` / `dodge_hold` | 闪避 / 长按闪避 | Shift 或 鼠标右键（双绑定） |
| `jump` / `jump_hold` | 跳跃 / 长按跳跃 | 空格 |
| `tool` | 工具 | T |
| `switch_1` ~ `switch_4` | 切换角色 | 1 / 2 / 3 / 4 |
| `finisher` | 处决 | F |
| `empty_action` | 空招式 | displayOnly，**跳过不执行** |
| `start_challenge` / `stop_recording` | 录制控制 | **跳过不执行**（录制语义） |

键位加载优先级：软件内用户配置 > `.wwkeys.json`（wwcombo 用户配置）> 上表默认。

### 5.3 轴库存储

本地目录管理（如 `%APPDATA%/wuwa-smartkey/charts/`），一个轴一个 JSON，附带导入时间、播放次数等元信息索引文件。

---

## 6. 播放引擎调度语义

### 6.1 执行模式

- **完整模式**（默认）：起手轴（`startup_axis`，起点固定 t=0）顺序执行 → 进入循环轴
- **仅起手 / 仅循环**：调试用
- **循环轮数上限**：默认无限，可配 N 轮后自动停止

### 6.2 步骤触发规则

| 参数 | 规则（默认） |
|---|---|
| 触发时刻 | `startMin`（可配策略：startMin / 区间中点 / startMax） |
| 按压时长 | `durationMin`；长按变体（`*_hold`）按住相应时长 |
| 主轨 `main` | 按时间轴顺序执行 |
| 独立轨 `independent` | 并行执行，不阻塞主轨 |
| 切人 | 下一步 `characterSlot` 与当前不同 → 提前发对应数字键，等待切换延迟（默认 300ms，可配） |
| 跳过 | `displayOnly`、录制控制类招式、`draft_period` |

### 6.3 循环周期边界

- 有 `periods`：循环周期 = `loop_axis` period 的 `endMs - startMs`，第 n 轮将循环段内 step 平移 `n × 周期`
- 无 `periods`（简化轴/简易宏）：退化用最后一个 main 轨 step 的 `startMin + durationMin` 作为周期

### 6.4 `free_fire`（自由发挥段）

播放器暂停推进，UI 提示"自由输出"，超时（默认 10s，可配）后自动进入下一段。不静默跳过——自由段承载蓄力/等 CD 语义。

### 6.5 控制与安全

- **倒计时启动**：按开始热键后默认 3s（可配）倒计时，给用户切回游戏的时间
- **紧急停止**：停止热键任何时刻生效；停止时对所有可能按住的键强制 keyup 兜底
- **不做暂停/恢复**（V1 明确砍掉）：恢复对齐语义复杂，"停了重来"已覆盖实际场景
- **失焦保护**：游戏窗口失去焦点 → 自动停止发键（防止按键打进其他程序）
- **窗口校验**：启动播放前检测鸣潮窗口存在，否则拒绝启动并提示
- **时间精度**：调度循环 `timeBeginPeriod(1)` + 高精度等待；预期抖动 1~2ms，远小于轴的时间粒度
- **倍速与偏移**（P1）：全局时间缩放 0.5~1.5x、±ms 偏移，用于不同机器的手感校准

---

## 7. 输入层

- **发送机制**：Win32 `SendInput` + `KEYEVENTF_SCANCODE`（扫描码）。鸣潮等 Unity 游戏过滤合成虚拟键消息，扫描码是被验证可行的路径（ok-ww / AHK 工具同路径）
- **鼠标**：同 API 的 `MOUSEINPUT`（普攻 = 左键点击，重击 = 左键按住）
- **全局热键**：`tauri-plugin-global-shortcut`（默认 F6 启动 / F7 停止，可配）
- **提权**：打包 manifest `requireAdministrator`
- **后台发键**：明确不支持，仅前台

---

## 8. 录制与识别（P1）

- **钩子**：Windows 低级钩子 `WH_KEYBOARD_LL` / `WH_MOUSE_LL`（`rdev` 或 windows-rs 直写），记录事件流：`(键码/鼠标键, 按下/抬起, 时间戳)`
- **注入过滤**：低级钩子可识别 `LLKH_INJECTED` 标志——回放期间录制不会录进本工具自己发出的键
- **事件 → 招式识别**：反查键位映射（5.2 表逆向）；按下→抬起间隔 ≥ 阈值（默认 300ms，可配）识别为长按变体；识别失败的按键保留原始标签存入 step
- **录制后清理**：一键删除空步、合并快速重复、按 `characterSlot` 归并切人
- 产出即标准 ComboChart，直接入库、可编辑、可分享

## 8b. 简易宏编辑器（P1，"按键精灵"核心体验）

表单式：动作列表（每行 = 招式或裸键 + 时长/间隔）+ 循环次数 → 生成单循环轴的 ComboChart。与完整轴共用引擎，无特殊逻辑。

---

## 9. Tauri 前后端接口

```text
Commands（前端 → Rust）
  list_charts() -> Vec<ChartMeta>
  import_chart(path) -> ChartMeta          # wwcombo JSON 导入
  delete_chart(id) / save_chart(chart)
  start_playback(chart_id, options)        # options: 模式/轮数/倍速/偏移
  stop_playback()
  start_recording() / stop_recording() -> ChartDraft
  get_keybindings() / set_keybindings()

Events（Rust → 前端，推送）
  playback:progress   # 当前步骤、轮次、时间
  playback:log        # 每步执行记录：计划时间/实际时间/偏差
  playback:stopped    # 停止原因（手动/失焦/完成）
```

UI 页面：轴库（列表+元信息）、播放面板（状态+控制+日志）、键位设置、简易宏编辑器、（P1）时间轴可视化、（P1）录制。

---

## 10. 功能优先级

### P0 — MVP（价值闭环：导入轴 → 游戏内自动连段）

播放引擎全量（6 节）、wwcombo 导入 + 轴库、键位映射（默认表 + `.wwkeys.json` 兼容 + 软件内可改）、热键启停/倒计时/紧急停止、失焦保护、提权、执行日志。

### P1 — 按键精灵化 + 好用

简易宏编辑器、录制（钩子 + 识别 + 清理）、干跑模式（不发键只打印计划）、时序报告（每步偏差统计）、倍速/偏移微调、时间轴可视化播放高亮。

### P2 — 编辑器与生态

完整轴编辑器（先做表格行式编辑，时间轴拖拽画布最后）、导出 wwcombo 兼容格式分享、手柄支持、在线轴库（远期）。

---

## 11. 里程碑

| 里程碑 | 内容 | 验收标准 |
|---|---|---|
| **M0 验证原型** | 纯 Rust bin（无 Tauri）：解析轴 JSON → SendInput 发键 | wwcombo 导出的轴，起手轴前三步（切人 → 普攻 → E）在游戏内正确生效 |
| **M1 引擎 + 壳** | 完整调度（起手/循环/独立轨/切人/free_fire）、热键、Tauri UI、轴库、键位映射、日志 | 任一导入轴可完整循环播放，紧急停止可靠 |
| **M2 按键精灵化** | 简易宏编辑器、录制、识别清理 | 从零录一段自己的连段 → 直接回放成功 |
| **M3 打磨** | 可视化、时序报告、微调、导出分享 | — |

M0 是风险最高的一步（发键是否被游戏接受、按压时长手感），**最先做**；其后每步都是纯增量。

---

## 12. 风险与对策

| 风险 | 对策 |
|---|---|
| 发键被游戏忽略 | 扫描码 + 管理员提权（ok-ww/AHK 已验证路径）；M0 最先验证 |
| 时序手感不对（机器延迟差异） | 时序报告定位偏差来源；倍速/偏移微调兜底 |
| wwcombo 格式版本演进 | serde 解析对未知字段宽容（不开 `deny_unknown_fields`）；关注其 `version` 字段 |
| 输入法/其他程序抢焦点 | 失焦保护自动停止；提权运行 |
| 账号风险 | 见免责声明；建议先以小号验证 |

## 13. 明确范围外

录制功能与轴编辑生态归 wwcombo 与本工具共同完成，但以下明确不做：游戏画面视觉识别/自动日常（ok-ww 领域）、后台窗口发键、暂停/恢复（V1）、在线服务。

## 14. 免责声明

本工具向游戏发送合成输入，可能违反游戏用户协议，存在账号处置风险。仅供个人学习研究，使用风险自担，请勿用于商业用途或代练等场景。

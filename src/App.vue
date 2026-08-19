<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

interface ChartMeta {
  id: string;
  title: string;
  character: string | null;
  tags: string[];
  file: string;
}
interface Step {
  id: string;
  moveId: string;
  label: string;
  characterSlot: number | null;
  lane: "main" | "independent";
  startMin: number;
  startMax: number;
  durationMin: number;
  color: string | null;
}
interface Period {
  id: string;
  kind: string;
  label: string | null;
  startMs: number;
  endMs: number;
}
interface Chart {
  id: string;
  title: string;
  steps: Step[];
  periods: Period[];
}
interface Hotkeys {
  start: string;
  stop: string;
  restart: string;
}

const charts = ref<ChartMeta[]>([]);
const selectedFile = ref<string | null>(null);
const chartDetail = ref<Chart | null>(null);
const playing = ref(false);
const loopsInput = ref<string>("");
const mode = ref<"full" | "startup" | "loop" | "semi">("full");
const logs = ref<string[]>([]);
const countdown = ref<number | null>(null);
const logBox = ref<HTMLElement | null>(null);

/// 半自动模式状态（等待切人 / 段播放中）
const semiState = ref<{
  phase: "wait" | "play";
  slot: number | null;
  round: number;
  index: number;
  total: number;
  steps: number;
  approxMs: number;
} | null>(null);

const hotkeys = ref<Hotkeys>({ start: "F6", stop: "F7", restart: "F8" });
const capturing = ref<keyof Hotkeys | null>(null);

/// 可视化：播放进度（映射到首轮坐标，百分比）
const progressPct = ref(0);
const activeStepId = ref<string | null>(null);

/// 时间轴缩放（1 = 自适应整轴宽度，>1 放大 + 横向滚动）
const zoom = ref(1);
const timelineScroll = ref<HTMLElement | null>(null);

function setZoom(z: number) {
  zoom.value = Math.min(200, Math.max(1, z));
}

function onWheel(e: WheelEvent) {
  e.preventDefault();
  setZoom(zoom.value * (e.deltaY < 0 ? 1.25 : 0.8));
}

// ---- 时间轴面板高度拖拽（借鉴 wwcombo timeline panel drag） ----
const timelineH = ref(220);
let tlDragY = 0;
let tlDragH = 0;

function beginTlDrag(e: PointerEvent) {
  tlDragY = e.clientY;
  tlDragH = timelineH.value;
  window.addEventListener("pointermove", onTlDrag);
  window.addEventListener("pointerup", endTlDrag);
  e.preventDefault();
}

function onTlDrag(e: PointerEvent) {
  timelineH.value = Math.min(700, Math.max(120, tlDragH + (e.clientY - tlDragY)));
}

function endTlDrag() {
  window.removeEventListener("pointermove", onTlDrag);
  window.removeEventListener("pointerup", endTlDrag);
}

// 播放时滚动视图自动跟随游标
watch(progressPct, (p) => {
  const el = timelineScroll.value;
  if (!el || zoom.value <= 1) return;
  const target = (p / 100) * el.scrollWidth;
  if (target < el.scrollLeft + 40 || target > el.scrollLeft + el.clientWidth - 60) {
    el.scrollLeft = Math.max(0, target - el.clientWidth * 0.3);
  }
});

let unlistenEv: UnlistenFn | null = null;
let unlistenHotkey: UnlistenFn | null = null;
let countdownTimer: ReturnType<typeof setInterval> | null = null;

// ---- 时间轴布局计算 ----
const totalMs = computed(() => {
  const c = chartDetail.value;
  if (!c) return 1;
  const byPeriods = c.periods.reduce((m, p) => Math.max(m, p.endMs), 0);
  const bySteps = c.steps.reduce((m, s) => Math.max(m, s.startMin + s.durationMin), 0);
  return Math.max(byPeriods, bySteps, 1);
});

const loopInfo = computed(() => {
  const c = chartDetail.value;
  const p = c?.periods.find((x) => x.kind === "loop_axis");
  if (!p) return null;
  return { start: p.startMs, len: p.endMs - p.startMs };
});

function pct(ms: number): number {
  return (ms / totalMs.value) * 100;
}

// 按角色槽位分行渲染（wwcombo 音游视图同款分道维度），
// 行内重叠的块做区间分层错开（不同操作互不遮盖），行高按层数自适应。
// 渲染读"编辑生效值"，未保存即可预览连锁平移效果。
const slotRows = computed(() => {
  const c = chartDetail.value;
  if (!c) return [];
  const slots = [...new Set(c.steps.map((s) => s.characterSlot ?? 0))].sort((a, b) => a - b);
  return slots.map((slot) => {
    const steps = c.steps
      .filter((s) => (s.characterSlot ?? 0) === slot)
      .slice()
      .sort((a, b) => editVal(a).startMin - editVal(b).startMin);
    // 区间分层：每个块放进第一个与其不重叠的层
    const layerEnds: number[] = [];
    const blocks = steps.map((s) => {
      const v = editVal(s);
      const left = pct(v.startMin);
      const width = Math.max(pct(v.durationMin), 0.35);
      let layer = layerEnds.findIndex((end) => left >= end);
      if (layer === -1) {
        layer = layerEnds.length;
        layerEnds.push(0);
      }
      layerEnds[layer] = left + width;
      return {
        id: s.id,
        label: s.label,
        independent: s.lane === "independent",
        left,
        width,
        layer,
        color: s.color ?? "#5a6270",
        title: `${s.label}${s.characterSlot ? ` · ${s.characterSlot}号位` : ""}${s.lane === "independent" ? " · 不占推进" : ""} @${v.startMin}ms`,
      };
    });
    return {
      slot,
      name: slot === 0 ? "通用" : `${slot}号位`,
      depth: Math.max(1, layerEnds.length),
      blocks,
    };
  });
});

// 时间刻度：随缩放自适应步长（每格约 70px），支持细粒度
const timeTicks = computed(() => {
  const total = totalMs.value;
  if (total <= 0) return [];
  const steps = [50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000, 30000, 60000];
  const pick = steps.find((s) => (s / total) * 100 * zoom.value * 12 >= 70) ?? 60000;
  const ticks: { left: number; label: string }[] = [];
  for (let t = 0; t <= total; t += pick) {
    ticks.push({ left: (t / total) * 100, label: `${(t / 1000).toFixed(pick < 1000 ? 2 : t % 1000 === 0 ? 0 : 1)}s` });
  }
  return ticks;
});

const periodMarks = computed(() => {
  const c = chartDetail.value;
  if (!c) return [];
  const names: Record<string, string> = {
    startup_axis: "起手",
    loop_axis: "循环",
    free_fire: "自由",
    draft_period: "草稿",
  };
  return c.periods.map((p) => ({
    label: `${names[p.kind] ?? p.kind}${p.label ? `:${p.label}` : ""}`,
    left: pct(p.startMs),
  }));
});

function appendLog(line: string) {
  logs.value.push(line);
  if (logs.value.length > 500) logs.value.splice(0, logs.value.length - 500);
  nextTick(() => logBox.value?.scrollTo({ top: logBox.value.scrollHeight }));
}

async function refresh() {
  charts.value = await invoke<ChartMeta[]>("list_charts");
  if (selectedFile.value && !charts.value.some((c) => c.file === selectedFile.value)) {
    select(null);
  }
}

async function select(c: ChartMeta | null) {
  selectedFile.value = c?.file ?? null;
  chartDetail.value = null;
  progressPct.value = 0;
  activeStepId.value = null;
  edits.value = {};
  invoke("set_current_chart", { file: selectedFile.value });
  if (c) await reloadChart();
}

async function reloadChart() {
  if (!selectedFile.value) return;
  try {
    chartDetail.value = await invoke<Chart>("get_chart", { file: selectedFile.value });
  } catch (e) {
    appendLog(`读取轴数据失败: ${e}`);
  }
}

async function importChart() {
  const path = await open({
    title: "选择 wwcombo 轴 JSON",
    filters: [{ name: "轴文件", extensions: ["json"] }],
  });
  if (typeof path === "string") {
    try {
      const meta = await invoke<ChartMeta>("import_chart", { path });
      appendLog(`已导入: ${meta.title}`);
      await refresh();
      const hit = charts.value.find((c) => c.file === meta.file);
      if (hit) select(hit);
    } catch (e) {
      appendLog(`导入失败: ${e}`);
    }
  }
}

async function removeChart() {
  if (!selectedFile.value) return;
  const c = charts.value.find((x) => x.file === selectedFile.value);
  if (!confirm(`删除轴「${c?.title}」？（仅从轴库移除，不影响原文件）`)) return;
  await invoke("delete_chart", { file: selectedFile.value });
  await refresh();
}

function parseLoops(): number | null {
  const n = parseInt(loopsInput.value, 10);
  return Number.isFinite(n) && n > 0 ? n : null;
}

async function start(modeOverride?: "full" | "startup" | "loop" | "semi") {
  if (!selectedFile.value || playing.value) return;
  const m = modeOverride ?? mode.value;
  try {
    playing.value = true;
    await invoke("start_playback", {
      file: selectedFile.value,
      loops: parseLoops(),
      mode: m,
      dryRun: false,
    });
    appendLog(m === "semi" ? "▶ 半自动开始：按数字键 1-4 切人，对应角色的段自动打出" : "▶ 开始播放");
  } catch (e) {
    playing.value = false;
    appendLog(`启动失败: ${e}`);
  }
}

async function stop() {
  cancelCountdown();
  await invoke("stop_playback");
}

function beginCountdown(modeOverride?: "full" | "startup" | "loop" | "semi") {
  if (!selectedFile.value || playing.value || countdown.value !== null) return;
  // 半自动开始后只等待切人键、不立即发键，无需倒计时
  if ((modeOverride ?? mode.value) === "semi") {
    start(modeOverride);
    return;
  }
  countdown.value = 3;
  appendLog("3 秒后开始，切到游戏窗口…（F7 取消）");
  countdownTimer = setInterval(() => {
    if (countdown.value === null) return;
    countdown.value -= 1;
    if (countdown.value <= 0) {
      cancelCountdown();
      start(modeOverride);
    }
  }, 1000);
}

function cancelCountdown() {
  if (countdownTimer) clearInterval(countdownTimer);
  countdownTimer = null;
  countdown.value = null;
}

// ---- 播放时钟：游标按真实时间平滑推进（用最近步骤事件做锚点校准） ----
let playTimer: ReturnType<typeof setInterval> | null = null;
let anchorPlanned = 0;
let anchorTime = 0;

function mapToFirst(ms: number): number {
  const li = loopInfo.value;
  if (li && li.len > 0 && ms >= li.start) {
    return li.start + ((ms - li.start) % li.len);
  }
  return ms;
}

function startPlayClock() {
  stopPlayClock();
  anchorPlanned = 0;
  anchorTime = Date.now();
  playTimer = setInterval(() => {
    const ms = mapToFirst(anchorPlanned + (Date.now() - anchorTime));
    progressPct.value = Math.min((ms / totalMs.value) * 100, 100);
  }, 100);
}

function stopPlayClock() {
  if (playTimer) clearInterval(playTimer);
  playTimer = null;
}

/// 播放事件 → 可视化状态
function updateVisualization(ev: Record<string, any>) {
  if (ev.Started) {
    startPlayClock();
    semiState.value = null;
  } else if (ev.StepDone) {
    // 半自动的 planned_ms 是段内相对时间，无法映射到全局轴，不推游标
    if (semiState.value) return;
    anchorPlanned = mapToFirst(ev.StepDone.planned_ms);
    anchorTime = Date.now();
    progressPct.value = (anchorPlanned / totalMs.value) * 100;
    const c = chartDetail.value;
    if (c) {
      let best: Step | null = null;
      for (const s of c.steps) {
        if (!best || Math.abs(s.startMin - anchorPlanned) < Math.abs(best.startMin - anchorPlanned)) best = s;
      }
      activeStepId.value = best?.id ?? null;
    }
  } else if (ev.Stopped) {
    stopPlayClock();
    progressPct.value = 0;
    activeStepId.value = null;
    semiState.value = null;
  } else if (ev.WaitingSwitch) {
    const w = ev.WaitingSwitch;
    semiState.value = {
      phase: "wait",
      slot: w.slot,
      round: w.round,
      index: w.index,
      total: w.total,
      steps: w.steps,
      approxMs: w.approx_ms,
    };
  } else if (ev.Switch && semiState.value) {
    // 半自动的段切换事件：切换到播放中状态（Started 之外的 Switch 属于半自动）
    semiState.value = { ...semiState.value, phase: "play", slot: ev.Switch.to || null };
  } else if (ev.SegmentDone) {
    if (semiState.value) semiState.value = { ...semiState.value, phase: "wait" };
  }
}

function fmtEvent(ev: Record<string, any>): string {
  if (ev.Started) return `▶ 开始: ${ev.Started.title}`;
  if (ev.LoopRound) return `── 循环第 ${ev.LoopRound.round} 轮 ──`;
  if (ev.Switch) return `  切人 → ${ev.Switch.to} 号位`;
  if (ev.FreeFire) return `  ○ 自由发挥段，等待 ${ev.FreeFire.wait_ms}ms`;
  if (ev.StepDone) {
    const s = ev.StepDone;
    const drift = s.actual_ms - s.planned_ms;
    return `  [${s.planned_ms}ms] ${s.label}（按住 ${s.held_ms}ms，偏差 ${drift >= 0 ? "+" : ""}${drift}ms）`;
  }
  if (ev.StepSkipped) return `  跳过 ${ev.StepSkipped.label}（无映射）`;
  if (ev.WaitingSwitch) {
    const w = ev.WaitingSwitch;
    const slot = w.slot != null ? `${w.slot} 号位` : "任意号位";
    return `⏳ 等待切人【${slot}】第 ${w.round} 轮 · 段 ${w.index}/${w.total}（${w.steps} 招 ≈ ${w.approx_ms}ms）`;
  }
  if (ev.SegmentDone) {
    const slot = ev.SegmentDone.slot != null ? `${ev.SegmentDone.slot} 号位` : "该";
    return ev.SegmentDone.reason === "switched"
      ? `↻ ${slot}段被切人打断`
      : `✓ ${slot}段打完`;
  }
  if (ev.KeyIgnored) {
    const exp = ev.KeyIgnored.expected != null ? `${ev.KeyIgnored.expected} 号位` : "任意号位";
    return `  ✗ 按了 ${ev.KeyIgnored.got} 号位（期望 ${exp}），已忽略`;
  }
  if (ev.Stopped) return ev.Stopped.reason === "manual" ? "■ 已停止" : "■ 播放完成";
  return JSON.stringify(ev);
}

// ---- 步骤编辑（时间/按住时长，毫秒） ----
const showEditor = ref(false);
const edits = ref<Record<string, { startMin: number; durationMin: number }>>({});

const editRows = computed(() =>
  (chartDetail.value?.steps ?? []).slice().sort((a, b) => a.startMin - b.startMin)
);

const dirtyCount = computed(
  () =>
    Object.entries(edits.value).filter(([id, e]) => {
      const s = chartDetail.value?.steps.find((x) => x.id === id);
      return s && (e.startMin !== s.startMin || e.durationMin !== s.durationMin);
    }).length
);

function editVal(s: Step) {
  return edits.value[s.id] ?? { startMin: s.startMin, durationMin: s.durationMin };
}

function isDirty(s: Step) {
  const e = edits.value[s.id];
  return !!e && (e.startMin !== s.startMin || e.durationMin !== s.durationMin);
}

/// 仅长按类招式（heavy_attack / *_hold）的按住时长由 durationMin 决定；
/// 点按类招式固定 40ms 点按，该字段不参与执行
function isHoldMove(moveId: string) {
  return moveId === "heavy_attack" || moveId.endsWith("_hold");
}

function onEditField(s: Step, field: "startMin" | "durationMin", v: string) {
  const n = parseInt(v, 10);
  if (!Number.isFinite(n) || n < 0) return;
  if (field === "durationMin") {
    const cur = editVal(s);
    edits.value[s.id] = { ...cur, durationMin: n };
    return;
  }
  // 连锁平移：修改某步时间，当前时间轴上位于其后的所有步骤整体平移相同差值，
  // 保持轴内相对节奏。基准始终是"当前编辑生效值"，反复调整/改回都不会错乱。
  const curStart = editVal(s).startMin;
  const delta = n - curStart;
  if (delta === 0) return;
  for (const st of chartDetail.value?.steps ?? []) {
    const cur = editVal(st);
    if (st.id === s.id) {
      edits.value[st.id] = { ...cur, startMin: n };
    } else if (cur.startMin >= curStart) {
      edits.value[st.id] = { ...cur, startMin: cur.startMin + delta };
    }
  }
}

async function saveEdits() {
  if (!selectedFile.value) return;
  const patches = Object.entries(edits.value)
    .filter(([id, e]) => {
      const s = chartDetail.value?.steps.find((x) => x.id === id);
      return s && (e.startMin !== s.startMin || e.durationMin !== s.durationMin);
    })
    .map(([id, e]) => ({ id, startMin: e.startMin, durationMin: e.durationMin }));
  if (!patches.length) return;
  try {
    const n = await invoke<number>("update_steps", { file: selectedFile.value, patches });
    edits.value = {};
    await reloadChart();
    appendLog(`已保存 ${n} 处步骤时间修改`);
  } catch (e) {
    appendLog(`保存失败: ${e}`);
  }
}

// ---- 热键设置 ----
async function beginCapture(which: keyof Hotkeys) {
  capturing.value = which;
}

async function onKeydown(e: KeyboardEvent) {
  if (!capturing.value) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.code === "Escape") {
    capturing.value = null;
    return;
  }
  const which = capturing.value;
  capturing.value = null;
  const next = { ...hotkeys.value, [which]: e.code } as Hotkeys;
  try {
    await invoke("set_settings", {
      settings: { hotkeyStart: next.start, hotkeyStop: next.stop, hotkeyRestart: next.restart },
    });
    hotkeys.value = next;
    appendLog(`热键已更新: 开始=${next.start} 停止=${next.stop} 循环重开=${next.restart}`);
  } catch (err) {
    appendLog(`热键设置失败: ${err}`);
  }
}

onMounted(async () => {
  await refresh();
  try {
    const s = await invoke<{ hotkeyStart: string; hotkeyStop: string; hotkeyRestart: string }>("get_settings");
    hotkeys.value = { start: s.hotkeyStart, stop: s.hotkeyStop, restart: s.hotkeyRestart };
  } catch {
    /* 默认 F6/F7/F8 */
  }
  window.addEventListener("keydown", onKeydown);

  unlistenEv = await listen("playback-event", (e) => {
    const ev = e.payload as Record<string, any>;
    appendLog(fmtEvent(ev));
    updateVisualization(ev);
    if (ev?.Stopped) playing.value = false;
  });
  unlistenHotkey = await listen("hotkey", (e) => {
    // 热键在游戏内按下，前台就是游戏，立即开始（倒计时对全屏游戏无反馈，反而像失灵）
    if (e.payload === "start") {
      appendLog("热键：立即开始");
      start();
    } else if (e.payload === "restart-loop") {
      cancelCountdown();
      playing.value = false;
      appendLog("热键：从循环轴立即重开");
      start("loop");
    } else if (e.payload === "stop") {
      cancelCountdown();
      appendLog("热键：停止");
    }
  });
});

onUnmounted(() => {
  unlistenEv?.();
  unlistenHotkey?.();
  cancelCountdown();
  stopPlayClock();
  endTlDrag();
  window.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <div class="app">
    <header>
      <h1>wuwa-smartkey</h1>
      <span class="hint">
        {{ hotkeys.start }} 开始 · {{ hotkeys.stop }} 停止 · {{ hotkeys.restart }} 从循环重开
      </span>
      <span class="status" :class="{ on: playing }">{{ playing ? "播放中" : "待命" }}</span>
    </header>

    <main>
      <section class="left">
        <div class="toolbar">
          <button @click="refresh">刷新</button>
          <button @click="importChart">导入轴…</button>
          <button :disabled="!selectedFile" @click="removeChart">删除</button>
        </div>
        <ul class="chart-list">
          <li
            v-for="c in charts"
            :key="c.file"
            :class="{ active: c.file === selectedFile }"
            @click="select(c)"
          >
            <div class="title">{{ c.title }}</div>
            <div class="meta">{{ c.character ?? "未知角色" }} · {{ c.file }}</div>
          </li>
          <li v-if="charts.length === 0" class="empty">轴库为空，点击「导入轴…」加载 wwcombo 导出的 JSON</li>
        </ul>
        <div class="hotkey-panel">
          <div class="panel-title">热键设置（点击后按下新键，Esc 取消）</div>
          <div class="hotkey-row">
            <span>开始</span>
            <button class="hk" :class="{ cap: capturing === 'start' }" @click="beginCapture('start')">
              {{ capturing === "start" ? "按下新键…" : hotkeys.start }}
            </button>
            <span>停止</span>
            <button class="hk" :class="{ cap: capturing === 'stop' }" @click="beginCapture('stop')">
              {{ capturing === "stop" ? "按下新键…" : hotkeys.stop }}
            </button>
            <span>循环重开</span>
            <button class="hk" :class="{ cap: capturing === 'restart' }" @click="beginCapture('restart')">
              {{ capturing === "restart" ? "按下新键…" : hotkeys.restart }}
            </button>
          </div>
        </div>
      </section>

      <section class="right">
        <div class="controls">
          <label>
            模式
            <select v-model="mode">
              <option value="full">完整</option>
              <option value="startup">仅起手</option>
              <option value="loop">仅循环</option>
              <option value="semi">半自动（手动切人）</option>
            </select>
          </label>
          <label>
            循环轮数
            <input v-model="loopsInput" placeholder="无限" />
          </label>
          <button class="primary" :disabled="!selectedFile || playing || countdown !== null" @click="beginCountdown()">
            开始
          </button>
          <button :disabled="!playing" @click="stop">停止</button>
          <button class="toggle-editor" :class="{ on: showEditor }" :disabled="!chartDetail" @click="showEditor = !showEditor">
            编辑步骤{{ dirtyCount ? ` (${dirtyCount})` : "" }}
          </button>
        </div>

        <div class="semi-bar" :class="semiState?.phase" v-if="semiState">
          <span class="semi-dot"></span>
          <template v-if="semiState.phase === 'wait'">
            等待切人 →【{{ semiState.slot ?? "任意" }} 号位】第 {{ semiState.round }} 轮 · 段
            {{ semiState.index }}/{{ semiState.total }}（{{ semiState.steps }} 招 ≈
            {{ (semiState.approxMs / 1000).toFixed(1) }}s）— 按对应数字键开打
          </template>
          <template v-else>
            【{{ semiState.slot ?? "?" }} 号位】段播放中…（按切人键可立即打断）
          </template>
        </div>

        <div class="timeline" v-if="chartDetail" :style="{ height: timelineH + 'px' }">
          <div class="tl-toolbar">
            <span class="tl-title">时间轴</span>
            <button class="tl-zoom" @click="setZoom(zoom / 1.25)">−</button>
            <span class="tl-zoomval">{{ zoom.toFixed(1) }}x</span>
            <button class="tl-zoom" @click="setZoom(zoom * 1.25)">＋</button>
            <button class="tl-zoom" @click="setZoom(1)">适配</button>
            <span class="tl-hint">滚轮缩放 · 拖下方横条调高度</span>
          </div>
          <div class="tl-grip" @pointerdown="beginTlDrag" title="上下拖拽调整时间轴高度"></div>
          <div ref="timelineScroll" class="tl-scroll" @wheel="onWheel">
            <div class="tl-canvas" :style="{ width: zoom * 100 + '%' }">
              <div class="tl-ticks">
                <div v-for="(t, i) in timeTicks" :key="i" class="tick" :style="{ left: t.left + '%' }">
                  {{ t.label }}
                </div>
              </div>
              <div v-for="m in periodMarks" :key="m.label" class="pmark" :style="{ left: m.left + '%' }">
                <span>{{ m.label }}</span>
              </div>
              <div
                v-for="row in slotRows"
                :key="row.slot"
                class="lane"
                :style="{ height: row.depth * 26 + 10 + 'px' }"
              >
                <div class="lane-body">
                  <div
                    v-for="b in row.blocks"
                    :key="b.id"
                    class="blk"
                    :class="{ active: b.id === activeStepId, indep: b.independent }"
                    :style="{ left: b.left + '%', width: b.width + '%', background: b.color, top: 5 + b.layer * 26 + 'px' }"
                    :title="b.title"
                  >
                    {{ b.label }}
                  </div>
                </div>
                <div class="lane-name">{{ row.name }}</div>
              </div>
              <div class="cursor" v-if="playing && progressPct > 0" :style="{ left: progressPct + '%' }"></div>
            </div>
          </div>
        </div>
        <div class="timeline empty-timeline" v-else>选择轴后显示时间轴</div>

        <div class="editor" v-if="chartDetail && showEditor">
          <div class="ed-head">
            <span class="ed-tip">修改某步时间后，其后的步骤会整体跟随平移（保持节奏）。「按住」仅对重击/长按类招式有效，点按类招式固定 40ms。start 与 duration 的 min/max 同步更新。</span>
            <button class="primary" :disabled="!dirtyCount" @click="saveEdits">
              保存修改{{ dirtyCount ? `（${dirtyCount}）` : "" }}
            </button>
          </div>
          <div class="ed-cols">
            <span>时间 (ms)</span><span>招式</span><span>位</span><span>按住 (ms)</span><span></span>
          </div>
          <div class="ed-rows">
            <div v-for="s in editRows" :key="s.id" class="ed-row" :class="{ dirty: isDirty(s) }">
              <input type="number" :value="Math.round(editVal(s).startMin)" @change="onEditField(s, 'startMin', ($event.target as HTMLInputElement).value)" />
              <span class="ed-label" :style="{ background: s.color ?? '#5a6270' }">{{ s.label }}</span>
              <span class="ed-slot">{{ s.characterSlot ?? "-" }}</span>
              <input
                v-if="isHoldMove(s.moveId)"
                type="number"
                :value="Math.round(editVal(s).durationMin)"
                @change="onEditField(s, 'durationMin', ($event.target as HTMLInputElement).value)"
              />
              <span v-else class="ed-tap" title="点按类招式固定 40ms 点按，durationMin 不参与执行">点按</span>
              <span class="ed-lane" :class="{ indep: s.lane === 'independent' }">
                {{ s.lane === "independent" ? "不占推进" : "" }}
              </span>
            </div>
          </div>
        </div>

        <div v-if="countdown !== null" class="countdown">{{ countdown }}</div>
        <div ref="logBox" class="logs">
          <div v-for="(l, i) in logs" :key="i" class="line">{{ l }}</div>
          <div v-if="logs.length === 0" class="empty">执行日志将显示在这里</div>
        </div>
      </section>
    </main>
  </div>
</template>

<style>
* { box-sizing: border-box; margin: 0; }
body { font-family: "Segoe UI", "Microsoft YaHei", sans-serif; background: #14161a; color: #e6e8ec; }
.app { display: flex; flex-direction: column; height: 100vh; }

header { display: flex; align-items: center; gap: 16px; padding: 12px 20px; background: #1b1e24; border-bottom: 1px solid #2a2e36; }
header h1 { font-size: 18px; font-weight: 600; }
.hint { color: #8a91a0; font-size: 13px; }
.status { margin-left: auto; padding: 2px 12px; border-radius: 10px; font-size: 13px; background: #2a2e36; color: #8a91a0; }
.status.on { background: #2e5e3a; color: #9fe6b5; }

main { display: flex; flex: 1; min-height: 0; }
.left { width: 330px; border-right: 1px solid #2a2e36; display: flex; flex-direction: column; }
.toolbar { display: flex; gap: 8px; padding: 10px; }
.chart-list { list-style: none; flex: 1; overflow-y: auto; padding: 0 10px 10px; }
.chart-list li { padding: 10px 12px; border-radius: 8px; cursor: pointer; }
.chart-list li:hover { background: #1f232b; }
.chart-list li.active { background: #26304a; }
.chart-list .title { font-size: 14px; }
.chart-list .meta { font-size: 12px; color: #8a91a0; margin-top: 2px; }
.chart-list .empty, .logs .empty, .empty-timeline { color: #6b7280; font-size: 13px; padding: 16px; text-align: center; }

.hotkey-panel { border-top: 1px solid #2a2e36; padding: 10px 12px; }
.panel-title { font-size: 12px; color: #8a91a0; margin-bottom: 8px; }
.hotkey-row { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; font-size: 12px; color: #b7bdc9; }
.hotkey-row .hk { min-width: 64px; padding: 4px 8px; font-size: 12px; }
.hotkey-row .hk.cap { background: #3b6ea5; border-color: #3b6ea5; animation: pulse 1s infinite; }
@keyframes pulse { 50% { opacity: .6; } }

.right { flex: 1; display: flex; flex-direction: column; min-width: 0; position: relative; }
.controls { display: flex; align-items: center; gap: 14px; padding: 12px 16px; border-bottom: 1px solid #2a2e36; }
.controls label { font-size: 13px; color: #b7bdc9; display: flex; align-items: center; gap: 6px; }
.controls input:not([type="checkbox"]), .controls select { width: 76px; padding: 4px 8px; border-radius: 6px; border: 1px solid #3a3f4a; background: #1f232b; color: #e6e8ec; }

/* 半自动状态条：等待切人（琥珀）/ 段播放中（绿） */
.semi-bar { display: flex; align-items: center; gap: 10px; padding: 10px 16px; font-size: 14px; border-bottom: 1px solid #2a2e36; }
.semi-bar.wait { background: #2b2517; color: #ffd97a; }
.semi-bar.play { background: #16281c; color: #9fe6b5; }
.semi-dot { width: 9px; height: 9px; border-radius: 50%; flex-shrink: 0; }
.semi-bar.wait .semi-dot { background: #ffd97a; animation: semi-pulse 1s ease-in-out infinite; }
.semi-bar.play .semi-dot { background: #9fe6b5; }
@keyframes semi-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.25; } }

.timeline { flex-shrink: 0; display: flex; flex-direction: column; border-bottom: 1px solid #2a2e36; padding-bottom: 4px; user-select: none; }
.tl-grip { height: 8px; margin: 0 12px; cursor: ns-resize; border-radius: 4px; background: #2a2e36; position: relative; flex-shrink: 0; }
.tl-grip::after { content: ""; position: absolute; left: 50%; top: 3px; width: 36px; height: 2px; transform: translateX(-50%); border-radius: 1px; background: #4a5160; }
.tl-grip:hover::after { background: #7d8598; }
.tl-toolbar { display: flex; align-items: center; gap: 6px; padding: 6px 12px 2px; }
.tl-title { font-size: 12px; color: #8a91a0; }
.tl-zoom { padding: 1px 9px; font-size: 12px; }
.tl-zoomval { font-size: 11px; color: #8a91a0; min-width: 36px; text-align: center; }
.tl-hint { font-size: 11px; color: #555c68; margin-left: auto; }
.tl-scroll { flex: 1; min-height: 0; overflow-x: auto; overflow-y: auto; padding: 0 12px; }
.tl-canvas { position: relative; min-width: 100%; }
.tl-ticks { position: relative; height: 16px; }
.tick { position: absolute; top: 0; font-size: 10px; color: #6b7280; transform: translateX(2px); border-left: 1px solid #333a46; padding-left: 3px; height: 100%; }
.pmark { position: absolute; top: 16px; bottom: 0; font-size: 10px; color: #9aa3b2; border-left: 1px dashed #4a5160; padding-left: 3px; pointer-events: none; }
.lane { position: relative; min-height: 36px; margin-top: 5px; }
.lane-body { position: absolute; inset: 0; background: #1a1d23; border-radius: 6px; overflow: hidden; }
.lane-name { position: absolute; left: 6px; top: 2px; font-size: 10px; color: #cbd2dc; background: rgba(20,22,26,.72); border-radius: 3px; padding: 0 4px; z-index: 3; pointer-events: none; }
/* 胶囊块（借鉴 wwcombo capsule），高度由行内层内联样式控制 */
.blk { position: absolute; height: 22px; border-radius: 999px; font-size: 10px; line-height: 22px; text-align: center; color: rgba(0,0,0,.8); overflow: hidden; white-space: nowrap; cursor: default; font-weight: 700; border: 1px solid rgba(255,255,255,.22); }
.blk.indep { opacity: .5; border: 1px dashed rgba(0,0,0,.5); }
.blk.active { outline: 2px solid #fff; box-shadow: 0 0 10px rgba(255,255,255,.9); z-index: 2; }
.cursor { position: absolute; top: 16px; bottom: 0; width: 2px; background: #9fe6b5; box-shadow: 0 0 6px #9fe6b5; z-index: 4; pointer-events: none; }

.editor { flex-shrink: 0; border-bottom: 1px solid #2a2e36; padding: 8px 12px; }
.ed-head { display: flex; align-items: center; gap: 12px; }
.ed-tip { font-size: 12px; color: #8a91a0; flex: 1; }
.ed-cols, .ed-row { display: grid; grid-template-columns: 90px 1fr 36px 90px 64px; gap: 8px; align-items: center; }
.ed-cols { font-size: 11px; color: #6b7280; padding: 8px 2px 2px; }
.ed-rows { max-height: 220px; overflow-y: auto; margin-top: 4px; }
.ed-row { padding: 2px; border-radius: 4px; }
.ed-row.dirty { background: #3a3320; }
.ed-row input { width: 100%; padding: 3px 6px; border-radius: 4px; border: 1px solid #3a3f4a; background: #1f232b; color: #e6e8ec; font-size: 12px; }
.ed-label { font-size: 12px; border-radius: 3px; padding: 1px 6px; color: rgba(0,0,0,.78); justify-self: start; }
.ed-slot { font-size: 12px; color: #8a91a0; text-align: center; }
.ed-lane { font-size: 10px; color: #6b7280; }
.ed-tap { font-size: 11px; color: #6b7280; text-align: center; cursor: help; }
.ed-lane.indep { color: #7d8598; font-style: italic; }
button.toggle-editor.on { background: #3b6ea5; border-color: #3b6ea5; }

.countdown { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); font-size: 96px; font-weight: 700; color: #9fe6b5; text-shadow: 0 0 40px rgba(0,0,0,.6); pointer-events: none; z-index: 10; }
.logs { flex: 1; overflow-y: auto; padding: 12px 16px; font-family: Consolas, "Courier New", monospace; font-size: 13px; line-height: 1.7; }
.logs .line:nth-child(odd) { background: rgba(255,255,255,.02); }

button { padding: 6px 14px; border-radius: 6px; border: 1px solid #3a3f4a; background: #262b33; color: #e6e8ec; cursor: pointer; font-size: 13px; }
button:hover:not(:disabled) { background: #303743; }
button:disabled { opacity: .45; cursor: not-allowed; }
button.primary { background: #3b6ea5; border-color: #3b6ea5; }
button.primary:hover:not(:disabled) { background: #4680bd; }
</style>

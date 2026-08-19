<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from "vue";
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
const dryRun = ref(false);
const mode = ref<"full" | "startup" | "loop">("full");
const logs = ref<string[]>([]);
const countdown = ref<number | null>(null);
const logBox = ref<HTMLElement | null>(null);

const hotkeys = ref<Hotkeys>({ start: "F6", stop: "F7", restart: "F8" });
const capturing = ref<keyof Hotkeys | null>(null);

/// 可视化：播放进度（映射到首轮坐标，百分比）
const progressPct = ref(0);
const activeStepId = ref<string | null>(null);

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

// 单条时间线：所有步骤（含 independent）混排，independent 为不占推进的辅助标记
const timelineBlocks = computed(() => {
  const c = chartDetail.value;
  if (!c) return [];
  return c.steps.map((s) => ({
    id: s.id,
    label: s.label,
    slot: s.characterSlot,
    independent: s.lane === "independent",
    left: pct(s.startMin),
    width: Math.max(pct(s.durationMin), 0.9),
    color: s.color ?? "#5a6270",
    title: `${s.label}${s.characterSlot ? ` · ${s.characterSlot}号位` : ""}${s.lane === "independent" ? " · 不占推进" : ""} @${s.startMin}ms`,
  }));
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
  invoke("set_current_chart", { file: selectedFile.value });
  if (c) {
    try {
      chartDetail.value = await invoke<Chart>("get_chart", { file: c.file });
    } catch (e) {
      appendLog(`读取轴数据失败: ${e}`);
    }
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

async function start(modeOverride?: "full" | "startup" | "loop") {
  if (!selectedFile.value || playing.value) return;
  try {
    playing.value = true;
    await invoke("start_playback", {
      file: selectedFile.value,
      loops: parseLoops(),
      mode: modeOverride ?? mode.value,
      dryRun: dryRun.value,
    });
    appendLog(dryRun.value ? "▶ 开始（干跑）" : "▶ 开始播放");
  } catch (e) {
    playing.value = false;
    appendLog(`启动失败: ${e}`);
  }
}

async function stop() {
  cancelCountdown();
  await invoke("stop_playback");
}

function beginCountdown(modeOverride?: "full" | "startup" | "loop") {
  if (!selectedFile.value || playing.value || countdown.value !== null) return;
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

/// 播放事件 → 可视化状态
function updateVisualization(ev: Record<string, any>) {
  if (ev.StepDone) {
    const planned: number = ev.StepDone.planned_ms;
    const li = loopInfo.value;
    let first = planned;
    if (li && li.len > 0 && planned >= li.start) {
      first = li.start + ((planned - li.start) % li.len);
    }
    progressPct.value = (first / totalMs.value) * 100;
    const c = chartDetail.value;
    if (c) {
      let best: Step | null = null;
      for (const s of c.steps) {
        if (!best || Math.abs(s.startMin - first) < Math.abs(best.startMin - first)) best = s;
      }
      activeStepId.value = best?.id ?? null;
    }
  } else if (ev.Stopped) {
    progressPct.value = 0;
    activeStepId.value = null;
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
  if (ev.Stopped) return ev.Stopped.reason === "manual" ? "■ 已停止" : "■ 播放完成";
  return JSON.stringify(ev);
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
            </select>
          </label>
          <label>
            循环轮数
            <input v-model="loopsInput" placeholder="无限" />
          </label>
          <label class="chk">
            <input type="checkbox" v-model="dryRun" />
            干跑（不发键）
          </label>
          <button class="primary" :disabled="!selectedFile || playing || countdown !== null" @click="beginCountdown()">
            开始
          </button>
          <button :disabled="!playing" @click="stop">停止</button>
        </div>

        <div class="timeline" v-if="chartDetail">
          <div class="period-marks">
            <div v-for="(m, i) in periodMarks" :key="i" class="pmark" :style="{ left: m.left + '%' }">
              <span>{{ m.label }}</span>
            </div>
          </div>
          <div class="lane">
            <div class="lane-name">轴</div>
            <div class="lane-body">
              <div
                v-for="b in timelineBlocks"
                :key="b.id"
                class="blk"
                :class="{ active: b.id === activeStepId, indep: b.independent }"
                :style="{ left: b.left + '%', width: b.width + '%', background: b.color }"
                :title="b.title"
              >
                {{ b.label }}
              </div>
            </div>
          </div>
          <div class="cursor" v-if="playing && progressPct > 0" :style="{ left: progressPct + '%' }"></div>
          <div class="scale">{{ (totalMs / 1000).toFixed(1) }}s</div>
        </div>
        <div class="timeline empty-timeline" v-else>选择轴后显示时间轴</div>

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

.timeline { position: relative; border-bottom: 1px solid #2a2e36; padding: 6px 16px 8px 62px; user-select: none; }
.period-marks { position: relative; height: 16px; }
.pmark { position: absolute; top: 0; font-size: 11px; color: #8a91a0; transform: translateX(2px); border-left: 1px dashed #4a5160; padding-left: 3px; height: 100%; }
.lane { display: flex; align-items: center; height: 26px; margin-top: 3px; }
.lane-name { position: absolute; left: 16px; width: 40px; font-size: 11px; color: #6b7280; }
.lane-body { position: relative; flex: 1; height: 100%; background: #1a1d23; border-radius: 4px; overflow: hidden; }
.blk { position: absolute; top: 3px; bottom: 3px; border-radius: 3px; font-size: 10px; line-height: 20px; text-align: center; color: rgba(0,0,0,.75); overflow: hidden; white-space: nowrap; cursor: default; }
.blk.indep { opacity: .55; border: 1px dashed rgba(0,0,0,.45); }
.blk.active { outline: 2px solid #fff; box-shadow: 0 0 8px rgba(255,255,255,.8); z-index: 2; }
.cursor { position: absolute; top: 22px; bottom: 14px; left: 62px; width: 2px; background: #9fe6b5; box-shadow: 0 0 6px #9fe6b5; z-index: 3; pointer-events: none; }
.scale { text-align: right; font-size: 11px; color: #6b7280; margin-top: 2px; }

.countdown { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); font-size: 96px; font-weight: 700; color: #9fe6b5; text-shadow: 0 0 40px rgba(0,0,0,.6); pointer-events: none; z-index: 10; }
.logs { flex: 1; overflow-y: auto; padding: 12px 16px; font-family: Consolas, "Courier New", monospace; font-size: 13px; line-height: 1.7; }
.logs .line:nth-child(odd) { background: rgba(255,255,255,.02); }

button { padding: 6px 14px; border-radius: 6px; border: 1px solid #3a3f4a; background: #262b33; color: #e6e8ec; cursor: pointer; font-size: 13px; }
button:hover:not(:disabled) { background: #303743; }
button:disabled { opacity: .45; cursor: not-allowed; }
button.primary { background: #3b6ea5; border-color: #3b6ea5; }
button.primary:hover:not(:disabled) { background: #4680bd; }
</style>

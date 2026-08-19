<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick } from "vue";
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

const charts = ref<ChartMeta[]>([]);
const selectedFile = ref<string | null>(null);
const playing = ref(false);
const loopsInput = ref<string>("");
const dryRun = ref(false);
const logs = ref<string[]>([]);
const countdown = ref<number | null>(null);
const logBox = ref<HTMLElement | null>(null);

let unlistenEv: UnlistenFn | null = null;
let unlistenHotkey: UnlistenFn | null = null;
let countdownTimer: ReturnType<typeof setInterval> | null = null;

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

function select(c: ChartMeta | null) {
  selectedFile.value = c?.file ?? null;
  invoke("set_current_chart", { file: selectedFile.value });
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

async function start() {
  if (!selectedFile.value || playing.value) return;
  try {
    playing.value = true;
    await invoke("start_playback", {
      file: selectedFile.value,
      loops: parseLoops(),
      mode: "full",
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

function beginCountdown() {
  if (!selectedFile.value || playing.value || countdown.value !== null) return;
  countdown.value = 3;
  appendLog("3 秒后开始，切到游戏窗口…（F7 取消）");
  countdownTimer = setInterval(() => {
    if (countdown.value === null) return;
    countdown.value -= 1;
    if (countdown.value <= 0) {
      cancelCountdown();
      start();
    }
  }, 1000);
}

function cancelCountdown() {
  if (countdownTimer) clearInterval(countdownTimer);
  countdownTimer = null;
  countdown.value = null;
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

onMounted(async () => {
  await refresh();
  unlistenEv = await listen("playback-event", (e) => {
    appendLog(fmtEvent(e.payload as Record<string, any>));
    if ((e.payload as any)?.Stopped) playing.value = false;
  });
  unlistenHotkey = await listen("hotkey", (e) => {
    if (e.payload === "start") beginCountdown();
    else if (e.payload === "stop") {
      cancelCountdown();
      appendLog("热键 F7：停止");
    }
  });
});

onUnmounted(() => {
  unlistenEv?.();
  unlistenHotkey?.();
  cancelCountdown();
});
</script>

<template>
  <div class="app">
    <header>
      <h1>wuwa-smartkey</h1>
      <span class="hint">F6 开始（3s 倒计时） · F7 停止</span>
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
      </section>

      <section class="right">
        <div class="controls">
          <label>
            循环轮数
            <input v-model="loopsInput" placeholder="无限" />
          </label>
          <label class="chk">
            <input type="checkbox" v-model="dryRun" />
            干跑（不发键）
          </label>
          <button class="primary" :disabled="!selectedFile || playing || countdown !== null" @click="start">
            开始
          </button>
          <button :disabled="!playing" @click="stop">停止</button>
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
.left { width: 320px; border-right: 1px solid #2a2e36; display: flex; flex-direction: column; }
.toolbar { display: flex; gap: 8px; padding: 10px; }
.chart-list { list-style: none; flex: 1; overflow-y: auto; padding: 0 10px 10px; }
.chart-list li { padding: 10px 12px; border-radius: 8px; cursor: pointer; }
.chart-list li:hover { background: #1f232b; }
.chart-list li.active { background: #26304a; }
.chart-list .title { font-size: 14px; }
.chart-list .meta { font-size: 12px; color: #8a91a0; margin-top: 2px; }
.chart-list .empty, .logs .empty { color: #6b7280; font-size: 13px; padding: 16px; text-align: center; }

.right { flex: 1; display: flex; flex-direction: column; min-width: 0; }
.controls { display: flex; align-items: center; gap: 14px; padding: 12px 16px; border-bottom: 1px solid #2a2e36; }
.controls label { font-size: 13px; color: #b7bdc9; display: flex; align-items: center; gap: 6px; }
.controls input:not([type="checkbox"]) { width: 70px; padding: 4px 8px; border-radius: 6px; border: 1px solid #3a3f4a; background: #1f232b; color: #e6e8ec; }
.countdown { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); font-size: 96px; font-weight: 700; color: #9fe6b5; text-shadow: 0 0 40px rgba(0,0,0,.6); pointer-events: none; }
.right { position: relative; }
.logs { flex: 1; overflow-y: auto; padding: 12px 16px; font-family: Consolas, "Courier New", monospace; font-size: 13px; line-height: 1.7; }
.logs .line:nth-child(odd) { background: rgba(255,255,255,.02); }

button { padding: 6px 14px; border-radius: 6px; border: 1px solid #3a3f4a; background: #262b33; color: #e6e8ec; cursor: pointer; font-size: 13px; }
button:hover:not(:disabled) { background: #303743; }
button:disabled { opacity: .45; cursor: not-allowed; }
button.primary { background: #3b6ea5; border-color: #3b6ea5; }
button.primary:hover:not(:disabled) { background: #4680bd; }
</style>

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";

// ---------------------------------------------------------------- types

type PlayMode = "one_shot" | "toggle" | "gate" | "loop";
type Action =
  | { type: "pad"; index: number }
  | { type: "bank"; index: number }
  | { type: "stop_all" }
  | { type: "master_gain" };

interface PadConfig { path: string; name: string; mode: PlayMode; gain: number }
interface Binding { kind: "note" | "cc"; channel: number; number: number; action: Action }
interface Settings {
  output_device: string | null;
  output_pair: number;
  buffer_frames: number | null;
  midi_input: string | null;
  midi_output: string | null;
  master_gain: number;
  banks: (PadConfig | null)[][];
  bindings: Binding[];
}
interface Snapshot {
  settings: Settings;
  bank: number;
  playing: boolean[];
  slot_status: ("empty" | "loading" | "ready" | "error")[];
  slot_errors: (string | null)[];
  audio: { device: string; sample_rate: number; channels: number } | null;
  audio_error: string | null;
  midi_input: string | null;
  midi_output: string | null;
  midi_error: string | null;
  learn: Action | null;
}
interface Devices { audio_outputs: string[]; midi_inputs: string[]; midi_outputs: string[] }

const PADS = 16;
const BANK_NAMES = ["A", "B", "C", "D"];
const MODE_LABEL: Record<PlayMode, string> = {
  one_shot: "ワンショット",
  toggle: "トグル",
  gate: "押している間",
  loop: "ループ",
};
// KeyboardEvent.code per pad (layout independent)
const KEY_CODES = [
  "Digit1", "Digit2", "Digit3", "Digit4", "KeyQ", "KeyW", "KeyE", "KeyR",
  "Digit5", "Digit6", "Digit7", "Digit8", "KeyT", "KeyY", "KeyU", "KeyI",
];
const KEY_LABELS = ["1", "2", "3", "4", "Q", "W", "E", "R", "5", "6", "7", "8", "T", "Y", "U", "I"];

// ---------------------------------------------------------------- state

let snap: Snapshot | null = null;
let devices: Devices = { audio_outputs: [], midi_inputs: [], midi_outputs: [] };
let selectedPad: number | null = null;
let dragOverPad: number | null = null;

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

// ---------------------------------------------------------------- pads

const padEls: HTMLDivElement[] = [];

function buildPads() {
  for (let i = 0; i < PADS; i++) {
    const el = document.createElement("div");
    el.className = "pad";
    el.dataset.pad = String(i);
    el.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      invoke("pad_press", { pad: i });
      const up = () => {
        invoke("pad_release", { pad: i });
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mouseup", up);
    });
    el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      selectedPad = selectedPad === i ? null : i;
      renderPads();
      renderEditor();
    });
    padEls.push(el);
    (i < 8 ? $("deck1") : $("deck2")).appendChild(el);
  }
}

function renderPads() {
  if (!snap) return;
  const bank = snap.bank;
  for (let i = 0; i < PADS; i++) {
    const slot = bank * PADS + i;
    const cfg = snap.settings.banks[bank][i];
    const status = snap.slot_status[slot];
    const el = padEls[i];
    el.classList.toggle("loaded", !!cfg);
    el.classList.toggle("playing", snap.playing[slot]);
    el.classList.toggle("selected", selectedPad === i);
    el.classList.toggle("dragover", dragOverPad === i);
    el.classList.toggle("error", status === "error");
    el.classList.toggle("learning", snap.learn?.type === "pad" && snap.learn.index === i);
    el.title = snap.slot_errors[slot] ?? cfg?.path ?? "";
    el.innerHTML = `
      <div class="pad-top"><span class="num">${(i % 8) + 1}</span><span class="key">${KEY_LABELS[i]}</span></div>
      <div class="pad-name">${cfg ? esc(cfg.name) : ""}</div>
      <div class="pad-bottom">${
        cfg
          ? `<span class="mode">${MODE_LABEL[cfg.mode]}</span>${
              status === "loading" ? '<span class="badge">読込中</span>' : ""
            }${status === "error" ? '<span class="badge err">エラー</span>' : ""}`
          : ""
      }</div>`;
  }
}

function setPlaying(slot: number, playing: boolean) {
  if (!snap) return;
  snap.playing[slot] = playing;
  const pad = slot - snap.bank * PADS;
  if (pad >= 0 && pad < PADS) padEls[pad].classList.toggle("playing", playing);
}

// ---------------------------------------------------------------- editor

function renderEditor() {
  const box = $("editor");
  if (!snap || selectedPad === null) {
    box.classList.add("hidden");
    return;
  }
  const bank = snap.bank;
  const pad = selectedPad;
  const cfg = snap.settings.banks[bank][pad];
  const deck = pad < 8 ? 1 : 2;
  const binding = snap.settings.bindings.find((b) => b.action.type === "pad" && b.action.index === pad);
  box.classList.remove("hidden");
  box.innerHTML = `
    <h3>バンク${BANK_NAMES[bank]} / DECK${deck} パッド${(pad % 8) + 1}</h3>
    <div class="form">
      <button id="ed-file">ファイルを選択…</button>
      ${
        cfg
          ? `<label>表示名 <input id="ed-name" value="${esc(cfg.name)}" /></label>
             <label>再生方法 <select id="ed-mode">${Object.entries(MODE_LABEL)
               .map(([k, v]) => `<option value="${k}" ${k === cfg.mode ? "selected" : ""}>${v}</option>`)
               .join("")}</select></label>
             <label>音量 <input id="ed-gain" type="range" min="0" max="2" step="0.01" value="${cfg.gain}" />
               <span id="ed-gain-val">${Math.round(cfg.gain * 100)}%</span></label>
             <button id="ed-clear" class="danger">割り当て解除</button>`
          : ""
      }
      <button id="ed-learn">MIDI学習</button>
      <span class="sub">MIDI: ${binding ? bindingLabel(binding) : "未割り当て"}</span>
      <button id="ed-close">閉じる</button>
    </div>
    ${cfg ? `<p class="sub path">${esc(cfg.path)}</p>` : ""}
    ${snap.slot_errors[bank * PADS + pad] ? `<p class="err">${esc(snap.slot_errors[bank * PADS + pad]!)}</p>` : ""}
  `;

  $("ed-file").onclick = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: "Audio", extensions: ["wav", "mp3", "flac", "ogg", "m4a", "aac", "aif", "aiff"] }],
    });
    if (typeof path === "string") await invoke("assign_pad", { bank, pad, path });
  };
  $("ed-learn").onclick = () => invoke("start_learn", { action: { type: "pad", index: pad } });
  $("ed-close").onclick = () => {
    selectedPad = null;
    renderPads();
    renderEditor();
  };
  if (cfg) {
    const push = () =>
      invoke("update_pad", {
        bank,
        pad,
        mode: ($("ed-mode") as HTMLSelectElement).value,
        gain: parseFloat(($("ed-gain") as HTMLInputElement).value),
        name: ($("ed-name") as HTMLInputElement).value,
      });
    $("ed-mode").onchange = push;
    $("ed-name").onchange = push;
    $("ed-gain").oninput = () => {
      $("ed-gain-val").textContent = Math.round(parseFloat(($("ed-gain") as HTMLInputElement).value) * 100) + "%";
      push();
    };
    $("ed-clear").onclick = () => invoke("clear_pad", { bank, pad });
  }
}

// ---------------------------------------------------------------- header / status

function renderHeader() {
  if (!snap) return;
  const banks = $("banks");
  banks.innerHTML = BANK_NAMES.map(
    (n, i) => `<button class="${i === snap!.bank ? "active" : ""}" data-bank="${i}" title="F${i + 1}">${n}</button>`,
  ).join("");
  banks.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    b.onclick = () => invoke("set_bank", { bank: Number(b.dataset.bank) });
  });
  const master = $<HTMLInputElement>("master");
  if (document.activeElement !== master) master.value = String(snap.settings.master_gain);
  $("master-val").textContent = Math.round(snap.settings.master_gain * 100) + "%";

  const parts: string[] = [];
  parts.push(
    snap.audio
      ? `<span class="ok">● 出力: ${esc(snap.audio.device)} (${snap.audio.sample_rate}Hz / ${snap.audio.channels}ch)</span>`
      : `<span class="ng">● 出力なし: ${esc(snap.audio_error ?? "")}</span>`,
  );
  parts.push(
    snap.midi_input
      ? `<span class="ok">● MIDI: ${esc(snap.midi_input)}</span>`
      : `<span class="warn">● MIDI未接続（キーボード操作のみ）</span>`,
  );
  if (snap.midi_error) parts.push(`<span class="ng">${esc(snap.midi_error)}</span>`);
  $("status").innerHTML = parts.join("");

  const banner = $("learn-banner");
  if (snap.learn) {
    banner.classList.remove("hidden");
    $("learn-text").textContent = `MIDI学習中: 「${actionLabel(snap.learn)}」 — 本体のボタン／つまみを操作してください`;
  } else {
    banner.classList.add("hidden");
  }
}

function actionLabel(a: Action): string {
  switch (a.type) {
    case "pad":
      return `DECK${a.index < 8 ? 1 : 2} パッド${(a.index % 8) + 1}`;
    case "bank":
      return `バンク${BANK_NAMES[a.index]}`;
    case "stop_all":
      return "全停止";
    case "master_gain":
      return "マスター音量";
  }
}

function bindingLabel(b: Binding) {
  return `${b.kind === "note" ? "Note" : "CC"} ch${b.channel + 1} 0x${b.number.toString(16).toUpperCase().padStart(2, "0")}`;
}

// ---------------------------------------------------------------- settings tab

function fillSelect(sel: HTMLSelectElement, options: [string, string][], value: string) {
  sel.innerHTML = options.map(([v, l]) => `<option value="${esc(v)}">${esc(l)}</option>`).join("");
  sel.value = value;
  if (sel.value !== value) {
    // saved device not present right now — keep it visible
    sel.insertAdjacentHTML("beforeend", `<option value="${esc(value)}">${esc(value)}（未接続）</option>`);
    sel.value = value;
  }
}

function renderSettings() {
  if (!snap) return;
  const s = snap.settings;
  fillSelect(
    $("audio-device"),
    [["", "自動（DDJがあればDDJ、なければ既定のデバイス）"], ...devices.audio_outputs.map((d) => [d, d] as [string, string])],
    s.output_device ?? "",
  );
  const ch = snap.audio?.channels ?? 2;
  const pairs: [string, string][] = [];
  for (let p = 0; p * 2 + 1 < Math.max(ch, 2); p++) pairs.push([String(p), `ch${p * 2 + 1}/${p * 2 + 2}`]);
  fillSelect($("audio-pair"), pairs, String(s.output_pair));
  ($("audio-buffer") as HTMLSelectElement).value = s.buffer_frames ? String(s.buffer_frames) : "";
  $("audio-info").textContent = snap.audio
    ? `使用中: ${snap.audio.device} / ${snap.audio.sample_rate}Hz / ${snap.audio.channels}ch（DDJ-RBのRCA出力は通常 ch1/2）`
    : `エラー: ${snap.audio_error ?? ""}`;

  fillSelect(
    $("midi-in"),
    [["", "自動（DDJを検索）"], ["-", "使わない"], ...devices.midi_inputs.map((d) => [d, d] as [string, string])],
    s.midi_input ?? "",
  );
  fillSelect(
    $("midi-out"),
    [["", "自動（DDJを検索）"], ["-", "使わない"], ...devices.midi_outputs.map((d) => [d, d] as [string, string])],
    s.midi_output ?? "",
  );

  $("bindings").innerHTML = s.bindings
    .map(
      (b, i) => `<tr>
        <td>${actionLabel(b.action)}</td>
        <td>${b.kind === "note" ? "Note" : "CC"}</td>
        <td>${b.channel + 1}</td>
        <td>0x${b.number.toString(16).toUpperCase().padStart(2, "0")}</td>
        <td><button data-del="${i}">削除</button></td></tr>`,
    )
    .join("");
  $("bindings").querySelectorAll<HTMLButtonElement>("button[data-del]").forEach((b) => {
    b.onclick = () => invoke("remove_binding", { index: Number(b.dataset.del) });
  });
}

async function refreshDevices() {
  devices = await invoke<Devices>("list_devices");
  renderSettings();
}

// ---------------------------------------------------------------- monitor

function describeMidi(bytes: number[]): string {
  const [st, d1, d2] = bytes;
  const ch = (st & 0x0f) + 1;
  const hex = (n: number) => "0x" + n.toString(16).toUpperCase().padStart(2, "0");
  switch (st & 0xf0) {
    case 0x90:
      return d2 > 0 ? `Note On  ch${ch} ${hex(d1)} vel ${d2}` : `Note Off ch${ch} ${hex(d1)}`;
    case 0x80:
      return `Note Off ch${ch} ${hex(d1)}`;
    case 0xb0:
      return `CC ch${ch} ${hex(d1)} = ${d2}`;
    default:
      return st === 0xf0 ? "SysEx" : "";
  }
}

function addMonitor(bytes: number[], action: string | null) {
  const tbody = $("monitor");
  const tr = document.createElement("tr");
  const t = new Date();
  tr.innerHTML = `<td>${t.toLocaleTimeString()}.${String(t.getMilliseconds()).padStart(3, "0")}</td>
    <td><code>${bytes.map((b) => b.toString(16).toUpperCase().padStart(2, "0")).join(" ")}</code></td>
    <td>${describeMidi(bytes)}</td><td>${action ? esc(action) : ""}</td>`;
  tbody.prepend(tr);
  while (tbody.children.length > 200) tbody.lastElementChild!.remove();
}

// ---------------------------------------------------------------- wiring

function render() {
  renderHeader();
  renderPads();
  renderEditor();
  renderSettings();
}

function isTyping() {
  const el = document.activeElement;
  return el instanceof HTMLInputElement && el.type !== "range" || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement;
}

function wireStatic() {
  document.querySelectorAll<HTMLButtonElement>("nav button").forEach((b) => {
    b.onclick = () => {
      document.querySelectorAll("nav button").forEach((x) => x.classList.toggle("active", x === b));
      document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("hidden", t.id !== `tab-${b.dataset.tab}`));
      if (b.dataset.tab === "settings") refreshDevices();
    };
  });

  $("master").addEventListener("input", (e) => {
    const g = parseFloat((e.target as HTMLInputElement).value);
    $("master-val").textContent = Math.round(g * 100) + "%";
    invoke("set_master", { gain: g });
  });
  $("stop-all").onclick = () => invoke("stop_all");
  $("learn-cancel").onclick = () => invoke("cancel_learn");

  $("audio-apply").onclick = () => {
    const dev = ($("audio-device") as HTMLSelectElement).value;
    const buf = ($("audio-buffer") as HTMLSelectElement).value;
    invoke("set_audio", {
      device: dev || null,
      outputPair: Number(($("audio-pair") as HTMLSelectElement).value),
      bufferFrames: buf ? Number(buf) : null,
    });
  };
  $("midi-apply").onclick = () => {
    const i = ($("midi-in") as HTMLSelectElement).value;
    const o = ($("midi-out") as HTMLSelectElement).value;
    invoke("set_midi", { input: i || null, output: o || null });
  };
  $("rescan").onclick = refreshDevices;
  document.querySelectorAll<HTMLButtonElement>("[data-learn-bank]").forEach((b) => {
    b.onclick = () => invoke("start_learn", { action: { type: "bank", index: Number(b.dataset.learnBank) } });
  });
  $("learn-stopall").onclick = () => invoke("start_learn", { action: { type: "stop_all" } });
  $("learn-master").onclick = () => invoke("start_learn", { action: { type: "master_gain" } });
  $("reset-bindings").onclick = () => {
    if (confirm("MIDI割り当てを初期値に戻しますか？")) invoke("reset_bindings");
  };
  $("monitor-clear").onclick = () => ($("monitor").innerHTML = "");

  // keyboard pads
  const down = new Set<string>();
  window.addEventListener("keydown", (e) => {
    if (isTyping()) return;
    const pad = KEY_CODES.indexOf(e.code);
    if (pad >= 0) {
      e.preventDefault();
      if (!down.has(e.code)) {
        down.add(e.code);
        invoke("pad_press", { pad });
      }
      return;
    }
    const fn = ["F1", "F2", "F3", "F4"].indexOf(e.code);
    if (fn >= 0) {
      e.preventDefault();
      invoke("set_bank", { bank: fn });
    } else if (e.code === "Escape") {
      if (snap?.learn) invoke("cancel_learn");
      else invoke("stop_all");
    }
  });
  window.addEventListener("keyup", (e) => {
    const pad = KEY_CODES.indexOf(e.code);
    if (pad >= 0 && down.delete(e.code)) invoke("pad_release", { pad });
  });
  window.addEventListener("blur", () => {
    for (const code of down) invoke("pad_release", { pad: KEY_CODES.indexOf(code) });
    down.clear();
  });
}

function padAt(x: number, y: number): number | null {
  const el = document.elementFromPoint(x, y)?.closest<HTMLElement>(".pad");
  return el ? Number(el.dataset.pad) : null;
}

async function main() {
  buildPads();
  wireStatic();

  await listen<Snapshot>("state", (e) => {
    snap = e.payload;
    render();
  });
  await listen<{ slot: number; playing: boolean }>("pad-state", (e) => setPlaying(e.payload.slot, e.payload.playing));
  await listen<{ bytes: number[]; action: string | null }>("midi", (e) => addMonitor(e.payload.bytes, e.payload.action));

  await getCurrentWebview().onDragDropEvent(async (e) => {
    const p = e.payload;
    if (p.type === "over" || p.type === "enter") {
      const r = window.devicePixelRatio;
      const pad = padAt(p.position.x / r, p.position.y / r);
      if (pad !== dragOverPad) {
        dragOverPad = pad;
        renderPads();
      }
    } else if (p.type === "drop") {
      const r = window.devicePixelRatio;
      const pad = padAt(p.position.x / r, p.position.y / r);
      dragOverPad = null;
      if (pad !== null && snap) {
        const bank = snap.bank;
        for (let k = 0; k < p.paths.length && pad + k < PADS; k++) {
          await invoke("assign_pad", { bank, pad: pad + k, path: p.paths[k] });
        }
      }
      renderPads();
    } else {
      dragOverPad = null;
      renderPads();
    }
  });

  snap = await invoke<Snapshot>("get_state");
  $("settings-path").textContent = await invoke<string>("settings_path");
  await refreshDevices();
  render();
}

main();

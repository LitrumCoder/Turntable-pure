const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const DISC_SPEED = 200;
const SKIN_CLASSES = ["skin-pioneer", "skin-braun", "skin-dual", "skin-studio"];

const SWAP_OUT_AT = 350;
const SWAP_OUT_TIME = 700;
const SWAP_IN_AT = 950;
const SWAP_IN_TIME = 650;
const SWAP_DROP_AT = 1600;
const SWAP_END = 2300;

const RESTING = "drop-shadow(0 0 0 rgba(0, 0, 0, 0))";
const LIFTED = "drop-shadow(5px 12px 9px rgba(0, 0, 0, 0.45))";
const AWAY = "translate3d(-135%, -16%, 14px) rotate(-14deg) scale(1.06)";
const HOVER = "translate3d(-3px, -7px, 14px) scale(1.06)";
const EASE_OUT = "cubic-bezier(0.2, 0.7, 0.3, 1)";
const EASE_IN = "cubic-bezier(0.5, 0, 0.8, 0.3)";

const OUT_FRAMES = [
  { transform: "none", filter: RESTING, opacity: 1, easing: EASE_OUT },
  { transform: HOVER, filter: LIFTED, opacity: 1, offset: 0.43, easing: EASE_IN },
  { transform: AWAY, filter: LIFTED, opacity: 0 },
];

const IN_FRAMES = [
  { transform: AWAY, filter: LIFTED, opacity: 0, easing: EASE_OUT },
  { transform: HOVER, filter: LIFTED, opacity: 1, offset: 0.6, easing: "ease-in-out" },
  { transform: "scale(0.985)", filter: RESTING, opacity: 1, offset: 0.85, easing: "ease-out" },
  { transform: "none", filter: RESTING, opacity: 1 },
];

const PERSPECTIVE = 1100;
const FIT_PAD = 6;
const ARM_LIFT = 4;
const VIEW_PRESETS = {
  top: { tilt: 0, yaw: 0 },
  perspective: { tilt: 58, yaw: 0 },
  angle: { tilt: 52, yaw: -28 },
  front: { tilt: 90, yaw: 0 },
};

const stage = document.getElementById("stage");
const deck = document.getElementById("deck");
const platter = deck.querySelector(".platter");
const slot = document.getElementById("slot");
const record = document.getElementById("record");
const label = document.getElementById("label");
const tonearm = document.getElementById("tonearm");
const caption = document.getElementById("caption");
const artistEl = document.getElementById("artist");
const titleEl = document.getElementById("title");
const combinedEl = document.getElementById("combined");
const frontCaption = document.getElementById("front-caption");

const armLayers = [tonearm];
for (let i = 1; i <= 2; i++) {
  const layer = tonearm.cloneNode(true);
  layer.removeAttribute("id");
  layer.querySelector("defs")?.remove();
  layer.classList.add("tonearm-layer");
  tonearm.after(layer);
  armLayers.push(layer);
}

let skin = "pioneer";
let arm = { rest: 24, start: 34.9, end: 53.9 };
let geo = { thick: 22, foot: 7, platterZ: 8, armZ: 14, armLength: 140 };
let view = { ...VIEW_PRESETS.top };
let viewGoal = { ...view };
let track = null;
let receivedAt = 0;
let discAngle = 0;
let discSpeed = 0;
let armAngle = arm.rest;
let armLift = 0;
let lastFrame = 0;
let animating = false;
let coverToken = 0;
let swapStart = null;

function trackKey(t) {
  return t ? [t.source, t.artist, t.title].join("\u001f") : "";
}

function currentPosition(now) {
  const elapsed = track.playing ? (now - receivedAt) / 1000 : 0;
  return track.position + elapsed;
}

function armTarget(now) {
  if (!track?.playing) return arm.rest;
  if (!track.duration) return arm.start;
  const progress = Math.min(1, Math.max(0, currentPosition(now) / track.duration));
  return arm.start + (arm.end - arm.start) * progress;
}

function placeArm() {
  armLayers.forEach((layer, i) => {
    layer.style.transform = `translateZ(${geo.armZ + armLift + i * 1.2}px) rotate(${armAngle}deg)`;
  });
  const standing = `rotate(${armAngle}deg) rotateZ(90deg) rotateX(90deg)`;
  deck.querySelector(".arm-side").style.transform = `translateZ(${geo.armZ + armLift}px) ${standing}`;
  deck.querySelector(".weight-side").style.transform = `translateZ(${geo.armZ + armLift - 3}px) ${standing}`;
}

function frame(now) {
  const dt = Math.min(0.1, (now - lastFrame) / 1000);
  lastFrame = now;

  if (swapStart !== null && now - swapStart >= SWAP_END) {
    swapStart = null;
    renderInfo();
  }
  const swapping = swapStart !== null;
  const away = swapping && now - swapStart < SWAP_DROP_AT;

  const target = track?.playing && !away ? DISC_SPEED : 0;
  discSpeed += (target - discSpeed) * Math.min(1, dt * (away ? 4 : 1.5));
  discAngle = (discAngle + discSpeed * dt) % 360;

  const armGoal = away ? arm.rest : armTarget(now);
  armAngle += (armGoal - armAngle) * Math.min(1, dt * (swapping ? 5 : 3));
  const lifted = swapping || Math.abs(armGoal - armAngle) > 0.3;
  armLift += ((lifted ? ARM_LIFT : 0) - armLift) * Math.min(1, dt * 8);

  record.style.transform = `translateZ(1.5px) rotate(${discAngle}deg)`;
  placeArm();
  tonearm.classList.toggle("lifted", lifted);

  const viewMoving = Math.abs(viewGoal.tilt - view.tilt) > 0.05 || Math.abs(viewGoal.yaw - view.yaw) > 0.05;
  if (viewMoving) {
    view.tilt += (viewGoal.tilt - view.tilt) * Math.min(1, dt * 9);
    view.yaw += (viewGoal.yaw - view.yaw) * Math.min(1, dt * 9);
    applyView();
  } else if (view.tilt !== viewGoal.tilt || view.yaw !== viewGoal.yaw) {
    view = { ...viewGoal };
    applyView();
    viewSettled();
  }

  const settled = discSpeed < 0.05 && Math.abs(armGoal - armAngle) < 0.01 && armLift < 0.01;
  if (settled && !track?.playing && !swapping && !viewMoving) {
    animating = false;
    return;
  }
  requestAnimationFrame(frame);
}

function wake() {
  if (animating) return;
  animating = true;
  lastFrame = performance.now();
  requestAnimationFrame(frame);
}

function project(x, y, z) {
  const w = deck.offsetWidth;
  const h = deck.offsetHeight;
  const tilt = (view.tilt * Math.PI) / 180;
  const yaw = (view.yaw * Math.PI) / 180;
  const dx = x - w / 2;
  const dy = y - h / 2;
  const rx = dx * Math.cos(yaw) - dy * Math.sin(yaw);
  const ry = dx * Math.sin(yaw) + dy * Math.cos(yaw);
  const py = ry * Math.cos(tilt) - z * Math.sin(tilt);
  const pz = ry * Math.sin(tilt) + z * Math.cos(tilt);
  const ox = stage.clientWidth / 2;
  const oy = stage.clientHeight / 2;
  const k = PERSPECTIVE / (PERSPECTIVE - pz);
  return [ox + (deck.offsetLeft + w / 2 + rx - ox) * k, oy + (deck.offsetTop + h / 2 + py - oy) * k];
}

function hull(points) {
  const pts = [...points].sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const cross = (o, a, b) => (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
  const lower = [];
  const upper = [];
  for (const p of pts) {
    while (lower.length > 1 && cross(lower.at(-2), lower.at(-1), p) <= 0) lower.pop();
    lower.push(p);
  }
  for (const p of pts.reverse()) {
    while (upper.length > 1 && cross(upper.at(-2), upper.at(-1), p) <= 0) upper.pop();
    upper.push(p);
  }
  return [...lower.slice(0, -1), ...upper.slice(0, -1)];
}

function outline() {
  const w = deck.offsetWidth;
  const h = deck.offsetHeight;
  const bottom = -(geo.thick + geo.foot);
  const points = [];
  for (const [x, y] of [[0, 0], [w, 0], [0, h], [w, h]]) {
    points.push(project(x, y, 0), project(x, y, bottom));
  }
  const r = platter.offsetWidth / 2;
  const cx = platter.offsetLeft + r;
  const cy = platter.offsetTop + r;
  for (let i = 0; i < 24; i++) {
    const a = (i / 24) * Math.PI * 2;
    points.push(project(cx + r * Math.cos(a), cy + r * Math.sin(a), geo.platterZ + 2));
  }
  const px = tonearm.offsetLeft;
  const py = tonearm.offsetTop;
  for (const [dx, dy] of [[-14, -14], [14, -14], [-14, 14], [14, 14]]) {
    points.push(project(px + dx, py + dy, geo.armZ + 8));
  }
  return hull(points);
}

function applyView() {
  deck.style.transform = `rotateX(${view.tilt}deg) rotateZ(${view.yaw}deg)`;
  for (const board of deck.querySelectorAll(".billboard")) {
    board.style.transform = `rotateZ(${-view.yaw}deg) rotateX(90deg)`;
  }
  deck.style.setProperty("--tilt-k", Math.sin((view.tilt * Math.PI) / 180).toFixed(3));
  deck.style.setProperty("--side-k", Math.min(1, Math.max(0, (view.tilt - 70) / 15)).toFixed(3));
  deck.classList.toggle("side-view", view.tilt >= 45);

  const shape = outline();
  const xs = shape.map((p) => p[0]);
  const ys = shape.map((p) => p[1]);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const maxY = Math.max(...ys);
  const minY = Math.min(...ys);
  const width = innerWidth;
  const height = innerHeight;
  const scale = Math.min(1, (width - 2 * FIT_PAD) / (maxX - minX), (height - 2 * FIT_PAD) / (maxY - minY));
  const tx = (width - (maxX - minX) * scale) / 2 - minX * scale;
  const ty = height - FIT_PAD - maxY * scale;
  stage.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
  return shape.map(([x, y]) => [tx + x * scale, ty + y * scale]);
}

function wrapYaw(yaw) {
  return ((((yaw + 180) % 360) + 360) % 360) - 180;
}

function viewSettled() {
  view.yaw = wrapYaw(view.yaw);
  viewGoal = { ...view };
  const shape = applyView();
  invoke("set_hit_region", { points: shape });
  try {
    localStorage.setItem(`vinyl.view.${skin}`, JSON.stringify(view));
  } catch {}
}

function loadView(name) {
  try {
    const saved = JSON.parse(localStorage.getItem(`vinyl.view.${name}`));
    if (saved && Number.isFinite(saved.tilt) && Number.isFinite(saved.yaw)) return saved;
  } catch {}
  return { ...VIEW_PRESETS.top };
}

function setViewGoal(goal) {
  viewGoal = {
    tilt: Math.min(90, Math.max(0, goal.tilt)),
    yaw: goal.yaw,
  };
  wake();
}

function addPart(className, css, before) {
  const part = document.createElement("div");
  part.className = className;
  part.style.cssText = css;
  before.before(part);
  return part;
}

function buildLayers() {
  deck.querySelectorAll(".edge-layer, .post-layer, .billboard, .arm-side, .weight-side").forEach((el) => el.remove());
  const r = platter.offsetWidth / 2;
  const px = tonearm.offsetLeft;
  const py = tonearm.offsetTop;
  addPart(
    "billboard platter-side",
    `left:${platter.offsetLeft}px;top:${platter.offsetTop + r}px;width:${2 * r}px;height:${geo.platterZ + 2}px`,
    platter,
  );
  addPart("billboard post-side", `left:${px - 4}px;top:${py}px;width:8px;height:${geo.armZ}px`, tonearm);
  addPart("arm-side", `left:${px}px;top:${py}px;width:${geo.armLength}px`, tonearm);
  addPart("weight-side", `left:${px - 22}px;top:${py}px`, tonearm);

  const size = platter.offsetWidth;
  for (let i = 0; i < geo.platterZ; i++) {
    addPart(
      "edge-layer",
      `left:${platter.offsetLeft}px;top:${platter.offsetTop}px;width:${size}px;height:${size}px;transform:translateZ(${i}px)`,
      platter,
    );
  }
  for (let i = 0; i < geo.armZ; i++) {
    addPart("post-layer", `left:${px - 4}px;top:${py - 4}px;transform:translateZ(${i}px)`, tonearm);
  }
}

function applySkin(name) {
  skin = name;
  deck.classList.remove(...SKIN_CLASSES);
  deck.classList.add(`skin-${name}`);

  const style = getComputedStyle(deck);
  const read = (prop) => parseFloat(style.getPropertyValue(prop));
  arm = { rest: read("--arm-rest"), start: read("--arm-start"), end: read("--arm-end") };
  geo = {
    thick: read("--thick"),
    foot: read("--foot"),
    platterZ: read("--platter-z"),
    armZ: read("--arm-z"),
    armLength: read("--arm-length"),
  };
  armAngle = armTarget(performance.now());
  buildLayers();
  placeArm();

  view = loadView(name);
  viewGoal = { ...view };
  viewSettled();
  updateMarquee();
  wake();
}

function updateMarquee() {
  for (const line of caption.querySelectorAll(".line")) {
    const text = line.firstElementChild;
    line.classList.remove("scroll");
    if (!line.offsetParent) continue;
    const overflow = line.scrollWidth - line.clientWidth;
    if (overflow > 2) {
      text.style.setProperty("--shift", `${-overflow}px`);
      text.style.setProperty("--duration", `${3 + overflow / 12}s`);
      line.classList.add("scroll");
    }
  }
}

function setText(el, text) {
  if (el.textContent === text) return false;
  el.textContent = text;
  return true;
}

function renderInfo() {
  const playing = Boolean(track?.playing);
  deck.classList.toggle("playing", playing);
  deck.classList.toggle("empty", !track);

  const artist = track?.artist ?? "";
  const title = track ? track.title : "тишина";
  const combined = track ? [artist, title].filter(Boolean).join(" — ") : "тишина";
  const changed = [setText(artistEl, artist), setText(titleEl, title), setText(combinedEl, combined)];
  setText(frontCaption, combined);
  if (changed.some(Boolean)) updateMarquee();
  caption.title = track ? combined : "";
  frontCaption.title = caption.title;
  caption.classList.toggle("idle", !track);
}

async function coverColor(url) {
  const img = new Image();
  img.src = url;
  await img.decode();

  const size = 24;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = size;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(img, 0, 0, size, size);
  const { data } = ctx.getImageData(0, 0, size, size);

  let r = 0, g = 0, b = 0, total = 0;
  for (let i = 0; i < data.length; i += 4) {
    const max = Math.max(data[i], data[i + 1], data[i + 2]);
    const min = Math.min(data[i], data[i + 1], data[i + 2]);
    const saturation = max ? (max - min) / max : 0;
    const weight = saturation * saturation * (max / 255) + 0.02;
    r += data[i] * weight;
    g += data[i + 1] * weight;
    b += data[i + 2] * weight;
    total += weight;
  }

  const rgb = [r, g, b].map((c) => c / total);
  const boost = 230 / Math.max(1, ...rgb);
  return rgb.map((c) => Math.round(Math.min(255, c * boost)));
}

function setCover(url) {
  const token = ++coverToken;
  label.style.backgroundImage = url ? `url("${url}")` : "";
  label.classList.toggle("blank", !url);
  label.classList.remove("fresh");
  void label.offsetWidth;
  label.classList.add("fresh");

  if (!url) {
    deck.classList.remove("lit");
    return;
  }
  coverColor(url)
    .then(([r, g, b]) => {
      if (token !== coverToken) return;
      deck.style.setProperty("--glow", `rgb(${r} ${g} ${b} / 0.9)`);
      deck.classList.add("lit");
    })
    .catch(() => deck.classList.remove("lit"));
}

function startSwap(hadRecord, hasRecord) {
  document.querySelectorAll(".slot.leaving").forEach((el) => el.remove());
  slot.getAnimations().forEach((a) => a.cancel());

  const skipped = hadRecord ? 0 : SWAP_IN_AT - 150;
  swapStart = performance.now() - skipped;

  if (hadRecord) {
    const leaving = slot.cloneNode(true);
    leaving.removeAttribute("id");
    leaving.querySelectorAll("[id]").forEach((el) => el.removeAttribute("id"));
    leaving.querySelector(".label").classList.remove("fresh");
    leaving.classList.add("leaving");
    slot.after(leaving);
    leaving
      .animate(OUT_FRAMES, { duration: SWAP_OUT_TIME, delay: SWAP_OUT_AT, fill: "both" })
      .finished.then(() => leaving.remove(), () => {});
  }
  if (hasRecord) {
    slot.animate(IN_FRAMES, { duration: SWAP_IN_TIME, delay: SWAP_IN_AT - skipped, fill: "backwards" });
  }
}

function togglePlayLocally() {
  if (!track) return;
  const now = performance.now();
  track.position = currentPosition(now);
  track.playing = !track.playing;
  receivedAt = now;
  renderInfo();
  wake();
}

listen("track", ({ payload }) => {
  const hadRecord = Boolean(track);
  const changed = trackKey(payload) !== trackKey(track);
  track = payload;
  receivedAt = performance.now();
  if (changed) startSwap(hadRecord, Boolean(track));
  renderInfo();
  wake();
});

listen("cover", ({ payload }) => setCover(payload));
listen("skin", ({ payload }) => applySkin(payload));
listen("view", ({ payload }) => {
  const preset = VIEW_PRESETS[payload];
  if (!preset) return;
  const turns = Math.round((viewGoal.yaw - preset.yaw) / 360);
  setViewGoal({ tilt: preset.tilt, yaw: preset.yaw + turns * 360 });
});

invoke("current_cover").then(setCover);
invoke("current_skin").then(applySkin);

const actions = {
  play: () => {
    invoke("toggle_play");
    togglePlayLocally();
  },
  next: () => invoke("next_track"),
  prev: () => invoke("prev_track"),
};

deck.addEventListener("click", (e) => {
  const button = e.target.closest("[data-action]");
  if (button) actions[button.dataset.action]();
});

deck.addEventListener("mousedown", (e) => {
  if (e.button === 0 && !e.target.closest("button")) {
    invoke("begin_drag", { grabX: e.clientX, grabY: e.clientY });
  }
});

deck.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    const delta = (e.deltaY || e.deltaX) * (e.deltaMode === 1 ? 33 : 1);
    if (e.shiftKey || e.ctrlKey) {
      setViewGoal({ tilt: viewGoal.tilt, yaw: viewGoal.yaw + delta * 0.08 });
    } else {
      setViewGoal({ tilt: viewGoal.tilt - delta * 0.05, yaw: viewGoal.yaw });
    }
  },
  { passive: false },
);

document.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  invoke("show_menu");
});

caption.addEventListener("transitionend", updateMarquee);
addEventListener("resize", () => viewSettled());

applySkin("pioneer");

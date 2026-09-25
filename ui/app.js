// Entry point: paste → probe (metadata + speculative download), Enter → job, polling, actions.
import { $, api, isLink, store } from "./fmt.js";
import { history, live, probeLine, trackChips } from "./view.js";

const opts = { format: store.get("format", "mp3"), quality: store.get("quality", "speech") };
const picked = new Set();
let probe = null, probedLink = "", jobId = null, audioFor = null, timer = null, historyKey = "";

function setSeg(name, value) {
  for (const b of document.querySelectorAll(`.seg[data-name="${name}"] button`)) b.classList.toggle("on", b.value === value);
}
setSeg("format", opts.format);
setSeg("quality", opts.quality);

document.querySelectorAll(".seg").forEach((seg) =>
  seg.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    if (!b) return;
    opts[seg.dataset.name] = b.value;
    store.set(seg.dataset.name, b.value);
    setSeg(seg.dataset.name, b.value);
  }));

$("more").addEventListener("click", () => {
  const open = $("params").hidden;
  $("params").hidden = !open;
  $("more").setAttribute("aria-expanded", String(open));
});

$("tracks").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-i]");
  if (!b) return;
  const i = Number(b.dataset.i);
  picked.has(i) ? picked.delete(i) : picked.add(i);
  b.classList.toggle("on");
});

// Several links pasted at once arrive as one line separated by spaces.
$("link").addEventListener("paste", (e) => {
  const text = e.clipboardData?.getData("text");
  if (!text || !/\s/.test(text.trim())) return;
  e.preventDefault();
  $("link").value = text.trim().split(/\s+/).join(" ");
  onLink();
});
$("link").addEventListener("input", () => {
  $("app").classList.toggle("typing", $("link").value.trim() !== "");
  onLink();
});

let probeTimer = null;
function onLink() {
  clearTimeout(probeTimer);
  const links = $("link").value.trim().split(/\s+/).filter(isLink);
  if (links.length !== 1 || links[0] === probedLink) return;
  probeTimer = setTimeout(async () => {
    probedLink = links[0];
    $("probe").textContent = "Читаю запись…";
    try {
      probe = await api("/api/probe", { link: probedLink, session_id: $("session").value || null });
      picked.clear();
      render({ probe });
      poll(1000);
    } catch (err) {
      probedLink = "";
      $("probe").textContent = err.message;
    }
  }, 250);
}

$("form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const links = $("link").value.trim();
  if (!links.split(/\s+/).some(isLink)) {
    $("probe").textContent = "Нужна ссылка вида …/record-new/<номер>";
    return;
  }
  try {
    const r = await api("/api/jobs", {
      links, session_id: $("session").value || null, format: opts.format, quality: opts.quality,
      from: $("from").value.trim(), to: $("to").value.trim(), tracks: [...picked],
    });
    jobId = r.ids[0];
    $("link").value = "";
    probedLink = "";
    poll(0);
  } catch (err) {
    $("probe").textContent = err.message;
  }
});

$("cancel").addEventListener("click", () => jobId && api(`/api/jobs/${jobId}/cancel`, {}).catch(() => {}));
document.addEventListener("click", (e) => {
  const b = e.target.closest("[data-reveal]");
  if (b) api("/api/reveal", { path: b.dataset.reveal }).catch(() => {});
});

function render(state) {
  if (state.probe !== undefined) {
    probe = state.probe ?? probe;
    $("probe").innerHTML = probeLine(probe);
    $("tracks").innerHTML = trackChips(probe, picked);
  }
  const job = state.job;
  const busy = !!job;
  $("app").className = (busy ? "busy" : "idle") + ($("link").value.trim() ? " typing" : "");
  $("live").hidden = !busy;
  if (job) {
    jobId = job.id;
    live(job, state.queued);
    // A growing stream while rendering, then the finished, seekable file — keeping the position.
    const player = $("player");
    const key = `${job.id}:${job.status === "done" ? "file" : "live"}`;
    if (job.audio && audioFor !== key) {
      const t = player.currentTime, playing = !player.paused;
      player.src = `/api/jobs/${job.id}/audio?${key}`;
      if (t) player.addEventListener("loadedmetadata", () => { player.currentTime = t; }, { once: true });
      if (playing) player.play().catch(() => {});
      audioFor = key;
    }
    player.hidden = !job.audio;
  }
  if (state.history) {
    const key = JSON.stringify(state.history);
    if (key !== historyKey) $("history").innerHTML = history(state.history), historyKey = key;
  }
}

async function poll(delay) {
  clearTimeout(timer);
  timer = setTimeout(async () => {
    let next = 2000;
    try {
      const s = await api("/api/state");
      render(s);
      if (s.job?.status === "running" || s.queued || (s.probe && s.probe.ready < s.probe.planned)) next = 500;
    } catch { /* the server is restarting — try again */ }
    poll(next);
  }, delay);
}

poll(0);
$("link").focus();

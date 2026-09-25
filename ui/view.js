// Rendering: the live job, per-track lanes, the latency histogram, history. Pure DOM, no framework.
import { $, esc, hms, mb, plural, speed } from "./fmt.js";

const STAGES = ["meta", "download", "mix", "done"];

export function probeLine(rec) {
  if (!rec) return "";
  const n = rec.tracks.length;
  const warm = rec.planned ? ` · прогрев ${Math.floor((100 * rec.ready) / rec.planned)}%` : "";
  return `<b>${esc(rec.title)}</b> · ${hms(rec.duration)} · ${n} ${plural(n, "дорожка", "дорожки", "дорожек")}${warm}`;
}

export function trackChips(rec, selected) {
  if (!rec) return `<span class="hint">появятся после вставки ссылки</span>`;
  return rec.tracks
    .map((t) => `<button type="button" data-i="${t.index}" class="${selected.has(t.index) ? "on" : ""}"
      title="${hms(t.start)}–${hms(t.start + t.duration)}">${t.index} · ${esc(t.name)}${t.host ? " ★" : ""}</button>`)
    .join("");
}

function lanes(rec) {
  const D = rec.duration || 1;
  return rec.tracks
    .filter((t) => t.total > 0)
    .map((t) => {
      const left = (100 * t.start) / D, width = Math.max((100 * t.duration) / D, 0.4);
      const f = t.total ? Math.min(1, t.done / t.total) : 0;
      return `<span class="n ${t.host ? "host" : ""}" title="${esc(t.name)}">${t.index} ${esc(t.name)}</span>
        <span class="t"><i style="left:${left}%;width:${width}%"><b style="width:${(f * 100).toFixed(1)}%"></b></i></span>
        <span class="p">${Math.floor(f * 100)}%</span>`;
    })
    .join("");
}

function histogram(bins) {
  const max = Math.max(1, ...bins), w = 96 / bins.length;
  return bins.map((b, i) => `<rect x="${(i * w + 0.5).toFixed(2)}" y="${(24 - (22 * b) / max).toFixed(2)}" width="${(w - 1).toFixed(2)}" height="${((22 * b) / max).toFixed(2)}"/>`).join("");
}

let lanesKey = "";

export function live(job, queued) {
  const rec = job.record;
  $("title").textContent = rec ? rec.title : "Читаю запись…";
  $("meta").textContent = rec ? `· ${hms(job.total || rec.duration)} · ${rec.tracks.length} ${plural(rec.tracks.length, "дорожка", "дорожки", "дорожек")}` : "";
  $("queue").textContent = queued ? `ещё ${queued} в очереди` : "";
  const running = job.status === "running";
  $("cancel").hidden = !running;
  const [a, aLabel, b, bLabel] =
    job.stage === "done" ? [hms(job.elapsed), "заняло", mb(job.out_bytes), job.files > 1 ? `${job.files} ${plural(job.files, "файл", "файла", "файлов")}` : "файл"]
    : job.stage === "mix" ? [job.realtime > 0 ? `×${Math.round(job.realtime)}` : "—", "к реальному времени", hms(job.eta), "осталось"]
    : [speed(job.speed), "МБ/с", hms(job.eta), "осталось"];
  $("speed").textContent = running || job.stage === "done" ? a : "—";
  $("speed").nextElementSibling.textContent = aLabel;
  $("eta").textContent = running || job.stage === "done" ? b : "—";
  $("eta").nextElementSibling.textContent = bLabel;
  $("pct").textContent = `${Math.floor(job.percent)}%`;
  const now = STAGES.indexOf(job.stage);
  for (const li of $("steps").children) {
    const i = STAGES.indexOf(li.dataset.s);
    li.className = i < now ? "past" : i === now ? "now" : "";
  }
  $("bar").style.width = `${job.percent}%`;
  if (rec) {
    const html = lanes(rec);
    if (html !== lanesKey) $("lanes").innerHTML = lanesKey = html;
  }
  $("hist").innerHTML = histogram(job.hist);
  $("pp").textContent = job.p50 != null ? `p50 ${job.p50.toFixed(1)} с · p99 ${job.p99.toFixed(1)} с · ожидание ≤ ${job.mixer_wait.toFixed(1)} с${job.http429 ? ` · 429×${job.http429}` : ""}` : "";
  $("error").hidden = !job.error;
  $("error").textContent = job.error ? `Ошибка: ${job.error}` : "";
  const done = job.status === "done";
  $("done").hidden = !done;
  if (done) {
    const name = job.path?.split("/").pop() || "";
    const label = /\.[a-z0-9]{2,5}$/i.test(name) ? name : `папка «${name}»`;
    $("done").innerHTML = `<b>Готово</b><span class="muted file" title="${esc(job.path)}">${esc(label)}</span>
      <button type="button" class="linkish" data-reveal="${esc(job.path)}">Показать в папке</button>`;
  }
}

export function history(items) {
  if (!items.length) return "";
  const rows = items.map((e) => `<li><span title="${esc(e.path)}">${esc(e.title)}</span>
    <span class="num">${esc(e.when)}</span><span class="num">${e.format.toUpperCase()} · ${mb(e.bytes)} · ${hms(e.took)}</span>
    <button type="button" class="linkish" data-reveal="${esc(e.path)}">показать</button></li>`);
  return `<h2>Недавние</h2><ul>${rows.join("")}</ul>`;
}

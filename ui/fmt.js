// Formatting helpers: time, sizes, speed. No dependencies.
export const $ = (id) => document.getElementById(id);

export const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

export function hms(sec) {
  if (sec == null || !isFinite(sec)) return "—";
  const s = Math.max(0, Math.round(sec));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(r)}` : `${m}:${pad(r)}`;
}

export function mb(bytes) {
  if (!bytes) return "0 МБ";
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(2)} ГБ` : `${(bytes / 1e6).toFixed(bytes >= 1e8 ? 0 : 1)} МБ`;
}

export const speed = (bps) => (bps > 0 ? (bps / 1e6).toFixed(1) : "—");

export const plural = (n, one, few, many) => {
  const d = n % 10, t = n % 100;
  return d === 1 && t !== 11 ? one : d >= 2 && d <= 4 && (t < 12 || t > 14) ? few : many;
};

export const isLink = (s) => /record-new\/\d+/.test(s);

// localStorage can be blocked (private windows) — preferences are a convenience only.
export const store = {
  get(k, d) { try { return localStorage.getItem(k) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, v); } catch { /* not important */ } },
};

export async function api(path, body) {
  const r = await fetch(path, body === undefined ? {} : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  if (r.status === 204) return null;
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
  return data;
}

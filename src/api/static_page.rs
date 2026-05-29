//! Home page: live FM station dashboard.
//!
//! The page subscribes to `/api/stations/stream` for a 1 Hz feed of
//! every scanned station, its current RDS metadata, and its active
//! listener count. While the user is *not* streaming any station the
//! idle-refresher in the backend cycles through the band so the
//! displayed metadata stays fresh. When the user hits play on a row
//! the page additionally opens a per-station SSE
//! (`/api/metadata/{khz}/stream`) so the now-playing text refreshes
//! at a faster cadence while the audio is up.

use axum::extract::State;
use axum::response::Html;

use crate::api::AppState;

pub async fn index(State(_state): State<AppState>) -> Html<&'static str> {
    Html(PAGE)
}

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>rtl-fm</title>
<style>
  body { font-family: system-ui, sans-serif; margin: 1.5rem auto; max-width: 980px; color: #1a1a1a; }
  h1 { margin: 0 0 .25rem; }
  .meta { color: #666; font-size: .85rem; margin-bottom: 1rem; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: .45rem .5rem; border-bottom: 1px solid #e2e2e2; vertical-align: middle; }
  th { font-weight: 600; font-size: .8rem; text-transform: uppercase; letter-spacing: .04em; color: #555; }
  tr.refreshing td { background: #f6faff; }
  tr.playing td { background: #f1fff1; }
  .freq { font-variant-numeric: tabular-nums; white-space: nowrap; }
  .ps { font-weight: 600; }
  .rt { color: #333; font-size: .9rem; max-width: 24rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pty { font-size: .8rem; color: #777; }
  .badge { display: inline-block; padding: .05rem .35rem; font-size: .7rem; background: #eef; color: #336; border-radius: .25rem; }
  .badge.live { background: #dfd; color: #161; }
  button { padding: .25rem .55rem; cursor: pointer; }
  audio { vertical-align: middle; height: 1.5rem; }
  small.dim { color: #999; }
  .toolbar { display: flex; gap: .5rem; align-items: center; margin-bottom: .75rem; }
</style>
</head>
<body>
<h1>rtl-fm</h1>
<div class="meta" id="meta">connecting…</div>

<div class="toolbar">
  <button id="rescan">Rescan band</button>
  <small class="dim">Idle refresher cycles through stations to keep metadata fresh.</small>
</div>

<table id="tbl">
  <thead>
    <tr>
      <th>Freq</th>
      <th>Callsign / PI</th>
      <th>PS</th>
      <th>RadioText</th>
      <th>PTY</th>
      <th>SNR</th>
      <th>Listen</th>
    </tr>
  </thead>
  <tbody id="rows"><tr><td colspan="7"><em>waiting for first scan…</em></td></tr></tbody>
</table>

<script>
const rowsEl = document.getElementById('rows');
const metaEl = document.getElementById('meta');
let currentFreq = null;
let currentAudio = null;
let perStationSse = null;

function fmtFreq(hz) { return (hz / 1e6).toFixed(1) + ' MHz'; }
function esc(s) { return (s ?? '').toString().replace(/[<>&"]/g, c => ({'<':'&lt;','>':'&gt;','&':'&amp;','"':'&quot;'}[c])); }

function render(snap) {
  metaEl.textContent =
    `tuner=${(snap.center_hz/1e6).toFixed(2)} MHz · window=[${(snap.window_lo_hz/1e6).toFixed(2)}..${(snap.window_hi_hz/1e6).toFixed(2)}] MHz · band=${snap.scan_band_mhz[0]}-${snap.scan_band_mhz[1]} MHz · ${snap.stations.length} stations`
    + (snap.idle_refresher_freq_hz ? ` · refreshing ${(snap.idle_refresher_freq_hz/1e6).toFixed(1)} MHz` : '');

  const idleFreq = snap.idle_refresher_freq_hz;
  rowsEl.innerHTML = snap.stations.map(s => {
    const m = s.metadata || {};
    const isRefreshing = idleFreq === s.freq_hz;
    const isPlaying = currentFreq === s.freq_hz;
    const cls = isPlaying ? 'playing' : (isRefreshing ? 'refreshing' : '');
    const khz = Math.round(s.freq_hz / 1000);
    const pi = m.pi_hex ? `<small class="dim">${m.pi_hex}</small>` : '';
    const call = m.callsign ? `<span class="badge live">${esc(m.callsign)}</span>` : '<small class="dim">–</small>';
    const ps = m.ps ? `<span class="ps">${esc(m.ps)}</span>` : '';
    const rt = m.radiotext ? `<span class="rt" title="${esc(m.radiotext)}">${esc(m.radiotext)}</span>` : '';
    const pty = m.pty_name ? `<span class="pty">${esc(m.pty_name)}</span>` : '';
    const listenBtn = isPlaying
      ? `<button data-stop>Stop</button>`
      : `<button data-play="${khz}">Play</button>`;
    return `<tr class="${cls}" data-freq="${s.freq_hz}">
      <td class="freq">${fmtFreq(s.freq_hz)}</td>
      <td>${call} ${pi}</td>
      <td>${ps}</td>
      <td>${rt}</td>
      <td>${pty}</td>
      <td><small class="dim">${(s.scan_power_db).toFixed(0)} dB</small></td>
      <td>${listenBtn}</td>
    </tr>`;
  }).join('') || '<tr><td colspan="7"><em>no stations yet — try Rescan band</em></td></tr>';
}

function applyPerStation(meta) {
  if (currentFreq == null) return;
  const tr = rowsEl.querySelector(`tr[data-freq="${currentFreq}"]`);
  if (!tr || !meta) return;
  const cells = tr.querySelectorAll('td');
  if (meta.callsign || meta.pi_hex) {
    cells[1].innerHTML =
      (meta.callsign ? `<span class="badge live">${esc(meta.callsign)}</span>` : '<small class="dim">–</small>')
      + (meta.pi_hex ? ` <small class="dim">${meta.pi_hex}</small>` : '');
  }
  if (meta.ps) cells[2].innerHTML = `<span class="ps">${esc(meta.ps)}</span>`;
  if (meta.radiotext) cells[3].innerHTML = `<span class="rt" title="${esc(meta.radiotext)}">${esc(meta.radiotext)}</span>`;
  if (meta.pty_name) cells[4].innerHTML = `<span class="pty">${esc(meta.pty_name)}</span>`;
}

function openSse() {
  const es = new EventSource('/api/stations/stream');
  es.onmessage = ev => {
    try { render(JSON.parse(ev.data)); }
    catch (e) { /* ignore */ }
  };
  es.onerror = () => { /* the browser will reconnect for us */ };
  return es;
}

document.addEventListener('click', async ev => {
  const playBtn = ev.target.closest('[data-play]');
  if (playBtn) {
    const khz = playBtn.dataset.play;
    stopAudio();
    const a = new Audio(`/stream/${khz}.flac?retune=true`);
    a.crossOrigin = 'anonymous';
    a.play().catch(() => {});
    currentAudio = a;
    currentFreq = parseInt(khz, 10) * 1000;
    // Open the per-station fast metadata stream.
    if (perStationSse) perStationSse.close();
    perStationSse = new EventSource(`/api/metadata/${khz}/stream`);
    perStationSse.onmessage = e => {
      try { applyPerStation(JSON.parse(e.data)); }
      catch (e) {}
    };
    return;
  }
  if (ev.target.closest('[data-stop]')) {
    stopAudio();
    return;
  }
  if (ev.target.id === 'rescan') {
    ev.target.disabled = true;
    try { await fetch('/api/rescan'); } catch {}
    ev.target.disabled = false;
  }
});

function stopAudio() {
  if (currentAudio) { currentAudio.pause(); currentAudio.src = ''; currentAudio = null; }
  currentFreq = null;
  if (perStationSse) { perStationSse.close(); perStationSse = null; }
}

openSse();
</script>
</body>
</html>
"#;

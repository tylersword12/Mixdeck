// Shared helpers for the player and remote pages.
'use strict';

async function api(path, opts = {}) {
  const init = { ...opts, headers: { 'Content-Type': 'application/json', ...(opts.headers || {}) } };
  if (init.body && typeof init.body !== 'string') init.body = JSON.stringify(init.body);
  const res = await fetch(path, init);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `Request failed (${res.status})`);
  return data;
}

function fmtTime(ms) {
  if (!ms || !isFinite(ms) || ms < 0) ms = 0;
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = String(s % 60).padStart(2, '0');
  return h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
}

function esc(s) {
  return String(s == null ? '' : s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

const SOURCE_LABEL = { spotify: 'Spotify', soundcloud: 'SoundCloud', youtube: 'YouTube' };

// Interpolated playback position from the last player report.
function livePosition(status) {
  if (!status) return 0;
  let p = status.position || 0;
  // _rx is set when this device received the report, so phone/host clock differences don't matter.
  if (status.playing) p += Date.now() - (status._rx || Date.now());
  return Math.min(p, status.duration || p);
}

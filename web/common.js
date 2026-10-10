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

// Transport icons drawn as SVG so they sit exactly centered in their buttons (text
// symbols like ▶ depend on the font and land off-center). The play triangle's centroid
// is at the middle of the box, which is where the eye expects it.
const ICON = {
  play: '<svg viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M8 5.6v12.8a1 1 0 0 0 1.52.85l10.4-6.4a1 1 0 0 0 0-1.7L9.52 4.75A1 1 0 0 0 8 5.6z"/></svg>',
  pause: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect fill="currentColor" x="6" y="5" width="4.2" height="14" rx="1.2"/><rect fill="currentColor" x="13.8" y="5" width="4.2" height="14" rx="1.2"/></svg>',
  prev: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect fill="currentColor" x="5" y="5.5" width="2.6" height="13" rx="1"/><path fill="currentColor" d="M19 6.6v10.8a1 1 0 0 1-1.53.85L9.2 13.1a1.3 1.3 0 0 1 0-2.2l8.27-5.15A1 1 0 0 1 19 6.6z"/></svg>',
  next: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect fill="currentColor" x="16.4" y="5.5" width="2.6" height="13" rx="1"/><path fill="currentColor" d="M5 6.6v10.8a1 1 0 0 0 1.53.85l8.27-5.15a1.3 1.3 0 0 0 0-2.2L6.53 5.75A1 1 0 0 0 5 6.6z"/></svg>',
};

const SOURCE_LABEL = { spotify: 'Spotify', soundcloud: 'SoundCloud', youtube: 'YouTube' };

// Interpolated playback position from the last player report.
function livePosition(status) {
  if (!status) return 0;
  let p = status.position || 0;
  // _rx is set when this device received the report, so phone/host clock differences don't matter.
  if (status.playing) p += Date.now() - (status._rx || Date.now());
  return Math.min(p, status.duration || p);
}

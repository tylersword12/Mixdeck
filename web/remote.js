// MixDeck remote: build and control the shared queue from any phone or computer.
'use strict';

const $ = s => document.querySelector(s);
const R = {
  state: { queue: [], currentUid: null, status: {}, settings: {} },
  info: null,
  view: null,
  searchSrc: 'spotify',
  libSrc: 'spotify',
  libStack: [],     // navigation inside Library / Playlists
  savedStack: [],
  seeking: false,
};
const IS_HOST = ['127.0.0.1', 'localhost', '[::1]'].includes(location.hostname);
// Inside the desktop app, links open in the default browser instead of a new app window.
function openExternal(url) {
  if (R.info && R.info.app && IS_HOST) return api('/api/open-url', { method: 'POST', body: { url } }).catch(e => toast(e.message, true));
  window.open(url, '_blank');
}
const WIDE = () => matchMedia('(min-width: 960px)').matches;

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------
let toastTimer;
function toast(msg, err = false) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = 'toast show' + (err ? ' err' : '');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.className = 'toast'; }, err ? 4000 : 1800);
}
async function cmd(action, extra = {}) {
  try { return await api('/api/cmd', { method: 'POST', body: { action, ...extra } }); }
  catch (e) { toast(e.message, true); throw e; }
}
function art(url, cls = 'art') { return `<div class="${cls}" style="background-image:url('${esc(url || '')}')"></div>`; }

const PLAYABLE_NOTE = { drm: 'Protected, can\'t play', unavailable: 'Unavailable', preview: '30s preview only' };

const GRIP = `<span class="grip" title="Drag to reorder" aria-label="Drag to reorder"><svg viewBox="0 0 10 16" width="10" height="16"><g fill="currentColor"><circle cx="2" cy="2" r="1.6"/><circle cx="8" cy="2" r="1.6"/><circle cx="2" cy="8" r="1.6"/><circle cx="8" cy="8" r="1.6"/><circle cx="2" cy="14" r="1.6"/><circle cx="8" cy="14" r="1.6"/></g></svg></span>`;

function trackRow(t, opts = {}) {
  const note = PLAYABLE_NOTE[t.playable] ? `<span class="warn"> · ${PLAYABLE_NOTE[t.playable]}</span>` : '';
  const cls = ['tr', opts.cls || '', ['drm', 'unavailable'].includes(t.playable) ? 'dim' : ''].join(' ');
  return `<li class="${cls}" data-i="${opts.i}">
    ${opts.grip ? GRIP : ''}${art(t.artwork)}
    <div class="m" data-act="${opts.tap || 'menu'}"><div class="t">${esc(t.title)}</div>
      <div class="a"><span class="badge ${t.source}">${SOURCE_LABEL[t.source]}</span> ${esc(t.artist)}${note}</div></div>
    <span class="d">${fmtTime(t.duration)}</span>
    ${opts.add === false ? '' : `<button class="ib" data-act="add" title="Add to queue">＋</button>`}
    <button class="ib" data-act="menu" title="More">⋯</button>
  </li>`;
}

// Render a track list and wire up its buttons. `tracks` is kept by reference for handlers.
function bindTrackList(container, tracks, menuFor) {
  container.querySelectorAll('.tr').forEach(li => {
    const t = tracks[Number(li.dataset.i)];
    li.querySelectorAll('[data-act]').forEach(el => el.addEventListener('click', e => {
      e.stopPropagation();
      const act = el.dataset.act;
      if (act === 'add') addTracks([t], 'end');
      else if (act === 'play') cmd('play', { uid: t.uid });
      else openSheet(t, menuFor(t));
    }));
  });
}

async function addTracks(tracks, where) {
  tracks = tracks.filter(t => !['drm', 'unavailable'].includes(t.playable));
  if (!tracks.length) return toast('Nothing playable to add', true);
  await cmd('add', { tracks, where, autostart: true });
  toast(where === 'now' ? 'Playing' : where === 'next' ? `Playing next: ${tracks.length > 1 ? tracks.length + ' tracks' : tracks[0].title}` : `Added ${tracks.length > 1 ? tracks.length + ' tracks' : tracks[0].title}`);
}

function browseMenu(t) {
  return [
    ['Play now', () => addTracks([t], 'now')],
    ['Play next', () => addTracks([t], 'next')],
    ['Add to end of queue', () => addTracks([t], 'end')],
    ['Add to a saved playlist…', () => pickSavedPlaylist([t])],
    t.url ? ['Open on ' + SOURCE_LABEL[t.source], () => openExternal(t.url)] : null,
  ];
}

// ---------------------------------------------------------------------------
// Bottom sheet
// ---------------------------------------------------------------------------
function openSheet(head, options) {
  const sheet = $('#sheet');
  const title = typeof head === 'string' ? `<div class="t">${esc(head)}</div>` :
    `<div class="t">${esc(head.title)}</div><div class="muted small">${esc(head.artist || '')}</div>`;
  const opts = options.filter(Boolean);
  sheet.innerHTML = `<div class="sh-title">${title}</div>` + opts.map((o, i) => `<button class="opt ${o[2] || ''}" data-i="${i}">${esc(o[0])}</button>`).join('') +
    `<button class="opt muted" data-close>Cancel</button>`;
  sheet.querySelectorAll('button').forEach(b => b.addEventListener('click', () => {
    closeSheet();
    if (b.dataset.i != null) opts[Number(b.dataset.i)][1]();
  }));
  $('#sheet-bg').classList.add('open');
  sheet.classList.add('open');
}
function closeSheet() { $('#sheet').classList.remove('open'); $('#sheet-bg').classList.remove('open'); }

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------
function show(view) {
  // Wide windows show the queue as a column, so "Now Playing" is its own page there;
  // narrow ones show now-playing above the queue.
  if (view === 'queue' && WIDE()) view = 'nowplaying';
  if (view === 'nowplaying' && !WIDE()) view = 'queue';
  R.view = view;
  document.body.dataset.view = view;
  document.querySelectorAll('.view').forEach(v => v.classList.toggle('active', v.id === 'v-' + view));
  document.querySelectorAll('#tabs button').forEach(b => b.classList.toggle('on', b.dataset.view === view));
  if (location.hash !== '#' + view) history.replaceState(null, '', '#' + view);
  if (view === 'library') renderLibrary();
  if (view === 'saved') renderSaved();
  if (view === 'settings') renderSettings();
  if (view === 'nowplaying') renderBigNp();
  if (view === 'search') setTimeout(() => { if (WIDE()) $('#search-q').focus(); }, 0);
  window.scrollTo(0, 0);
}

// ---- Now playing + queue -----------------------------------------------------
function currentTrack() { return R.state.queue.find(t => t.uid === R.state.currentUid); }

function renderNowPlaying() {
  const t = currentTrack();
  const st = R.state.status || {};
  const np = $('#np');
  if (!t) {
    np.innerHTML = `<div class="np"><div class="muted">${R.state.queue.length ? 'Tap a track in the queue to start.' : 'Your queue is empty. Search or open your library to add music.'}</div>
      ${R.state.queue.length ? `<div class="ctl"><button class="round primary" id="np-play" title="Play">${ICON.play}</button></div>` : ''}</div>`;
  } else {
    const trans = st.transition ? `<div class="pill wait small">Crossfading…</div>` : st.message ? `<div class="pill wait small">${esc(st.message)}</div>` : '';
    np.innerHTML = `<div class="np"><div class="np-top">${art(t.artwork)}<div style="min-width:0;flex:1">
      <div class="lbl"><span class="badge ${t.source}">${SOURCE_LABEL[t.source]}</span></div>
      <div class="t">${esc(t.title)}</div><div class="a">${esc(t.artist)}</div>${trans}</div></div>
      <input type="range" id="np-seek" min="0" max="${st.duration || t.duration || 1}" value="${livePosition(st)}" step="1000">
      <div class="times"><span id="np-pos">${fmtTime(livePosition(st))}</span><span>${fmtTime(st.duration || t.duration)}</span></div>
      <div class="ctl">
        <button class="round" id="np-prev" title="Previous">${ICON.prev}</button>
        <button class="round primary" id="np-play" title="${st.playing ? 'Pause' : 'Play'}">${st.playing ? ICON.pause : ICON.play}</button>
        <button class="round" id="np-next" title="Next">${ICON.next}</button>
      </div>
      <div class="vol" style="justify-content:center;margin-top:10px">Vol <input id="np-vol" type="range" min="0" max="1" step="0.01" value="${st.volume ?? 1}"></div></div>`;
    const seek = $('#np-seek');
    seek.addEventListener('input', () => { R.seeking = true; $('#np-pos').textContent = fmtTime(Number(seek.value)); });
    seek.addEventListener('change', () => { cmd('seek', { ms: Number(seek.value) }).finally(() => { R.seeking = false; }); });
    $('#np-prev').onclick = () => cmd('prev');
    $('#np-next').onclick = () => cmd('next');
    $('#np-vol').addEventListener('change', e => cmd('volume', { value: Number(e.target.value) }));
  }
  const play = $('#np-play');
  if (play) play.onclick = () => cmd(t ? 'toggle' : 'play');
  renderMini();
  if (R.view === 'nowplaying') renderBigNp();
}

// The Now Playing page on wide windows: big artwork, controls and what's up next.
function renderBigNp() {
  const el = $('#bignp');
  if (!el) return;
  const t = currentTrack();
  const st = R.state.status || {};
  const q = R.state.queue;
  if (!t) {
    el.innerHTML = `<div class="bnp-empty">${q.length ? 'Nothing is playing. Pick a track in the queue, or press play.' : 'Nothing is playing. Search or open your library to add music.'}
      ${q.length ? `<div class="ctl" style="display:flex;justify-content:center;margin-top:14px"><button class="round primary" id="bnp-play" title="Play">${ICON.play}</button></div>` : ''}</div>`;
    const p = $('#bnp-play'); if (p) p.onclick = () => cmd('play');
    return;
  }
  const ci = q.findIndex(x => x.uid === t.uid);
  const upNext = q.slice(ci + 1, ci + 9);
  const note = st.transition ? `<div class="pill wait small" style="margin-top:8px">Crossfading…</div>` : st.message ? `<div class="pill wait small" style="margin-top:8px">${esc(st.message)}</div>` : '';
  const dur = st.duration || t.duration || 1;
  el.innerHTML = `<div class="bnp">${art(t.artwork, 'bart')}<div class="binfo">
      <div class="lbl"><span class="badge ${t.source}">${SOURCE_LABEL[t.source]}</span></div>
      <div class="t">${esc(t.title)}</div><div class="a">${esc(t.artist)}</div>${note}
      <input type="range" class="seek" id="bnp-seek" min="0" max="${dur}" value="${livePosition(st)}" step="1000">
      <div class="times"><span id="bnp-pos">${fmtTime(livePosition(st))}</span><span>${fmtTime(st.duration || t.duration)}</span></div>
      <div class="ctl">
        <button class="round" id="bnp-prev" title="Previous">${ICON.prev}</button>
        <button class="round primary" id="bnp-play" title="${st.playing ? 'Pause' : 'Play'}">${st.playing ? ICON.pause : ICON.play}</button>
        <button class="round" id="bnp-next" title="Next">${ICON.next}</button>
      </div>
      <div class="vol">Vol <input id="bnp-vol" type="range" min="0" max="1" step="0.01" value="${st.volume ?? 1}"></div>
    </div></div>
    <h3>Up next</h3>
    ${upNext.length ? '<ul class="list" id="bnp-next-list"></ul>' : '<div class="empty-note">Nothing queued after this track.</div>'}`;
  const seek = $('#bnp-seek');
  seek.addEventListener('input', () => { R.seeking = true; $('#bnp-pos').textContent = fmtTime(Number(seek.value)); });
  seek.addEventListener('change', () => { cmd('seek', { ms: Number(seek.value) }).finally(() => { R.seeking = false; }); });
  $('#bnp-prev').onclick = () => cmd('prev');
  $('#bnp-next').onclick = () => cmd('next');
  $('#bnp-play').onclick = () => cmd('toggle');
  $('#bnp-vol').addEventListener('change', e => cmd('volume', { value: Number(e.target.value) }));
  const ul = $('#bnp-next-list');
  if (ul) {
    const nextUid = st.next && st.next.uid;
    ul.innerHTML = upNext.map((x, k) => trackRow(x, { i: ci + 1 + k, tap: 'play', add: false }).replace('<span class="d">',
      x.uid === nextUid ? `<span class="d">${st.next.ready ? '● ' : '◌ '}` : '<span class="d">')).join('');
    bindTrackList(ul, q, queueMenu);
  }
}

function renderMini() {
  const t = currentTrack();
  const st = R.state.status || {};
  const mini = $('#mini');
  if (!t) { mini.style.display = 'none'; return; }
  mini.style.display = '';
  const pct = st.duration ? Math.min(100, (livePosition(st) / st.duration) * 100) : 0;
  mini.innerHTML = `<div class="prog" style="width:${pct}%"></div>${art(t.artwork)}
    <div class="m"><div class="t">${esc(t.title)}</div><div class="a">${st.message ? `<span style="color:var(--warn)">${esc(st.message)}…</span>` : esc(t.artist)}</div></div>
    <button id="mini-play" title="${st.playing ? 'Pause' : 'Play'}">${st.playing ? ICON.pause : ICON.play}</button><button id="mini-next" title="Next">${ICON.next}</button>`;
  mini.querySelector('.m').onclick = () => show('queue');
  $('#mini-play').onclick = () => cmd('toggle');
  $('#mini-next').onclick = () => cmd('next');
}

function tickProgress() {
  const st = R.state.status || {};
  if (!st.playing) return;
  const pos = livePosition(st);
  if (!R.seeking && $('#np-seek')) { $('#np-seek').value = pos; $('#np-pos').textContent = fmtTime(pos); }
  if (!R.seeking && $('#bnp-seek')) { $('#bnp-seek').value = pos; $('#bnp-pos').textContent = fmtTime(pos); }
  const p = $('#mini .prog');
  if (p && st.duration) p.style.width = Math.min(100, (pos / st.duration) * 100) + '%';
}

function renderQueue() {
  if (R.drag) { R.drag.pending = true; return; } // don't rebuild the list under a finger mid-drag
  const q = R.state.queue;
  const ci = q.findIndex(t => t.uid === R.state.currentUid);
  const ul = $('#queue');
  const nextUid = R.state.status && R.state.status.next && R.state.status.next.uid;
  if (!q.length) { ul.innerHTML = ''; return; }
  ul.innerHTML = q.map((t, i) => {
    let cls = i === ci ? 'cur' : ci >= 0 && i < ci ? 'played' : '';
    return trackRow(t, { i, cls, tap: 'play', add: false, grip: true }).replace('<span class="d">',
      t.uid === nextUid ? `<span class="d">${R.state.status.next.ready ? '● ' : '◌ '}` : '<span class="d">');
  }).join('');
  bindTrackList(ul, q, queueMenu);
  ul.querySelectorAll('.grip').forEach(g => g.addEventListener('pointerdown', e => startDrag(e, g, ul)));
}

function queueMenu(t) {
    const i = R.state.queue.findIndex(x => x.uid === t.uid);
    return [
      ['Play now', () => cmd('play', { uid: t.uid })],
      ['Play next', () => cmd('move', { uid: t.uid, to: 'next' })],
      i > 0 ? ['Move up', () => cmd('move', { uid: t.uid, to: i - 1 })] : null,
      i < R.state.queue.length - 1 ? ['Move down', () => cmd('move', { uid: t.uid, to: i + 1 })] : null,
      ['Move to top', () => cmd('move', { uid: t.uid, to: 0 })],
      ['Add to a saved playlist…', () => pickSavedPlaylist([t])],
      ['Remove from queue', () => cmd('remove', { uid: t.uid }), 'danger'],
    ];
}

// ---- Drag to reorder ---------------------------------------------------------
// Works with mouse and touch: grab the dotted handle on the left of a queue row.
function startDrag(e, grip, ul) {
  if (e.button > 0) return;
  e.preventDefault();
  e.stopPropagation();
  const li = grip.closest('.tr');
  const rows = [...ul.children];
  const from = rows.indexOf(li);
  if (from < 0 || rows.length < 2) return;
  const wide = WIDE();
  const scroller = wide ? $('#v-queue') : (document.scrollingElement || document.documentElement);
  const scrollTop = () => scroller.scrollTop;
  const startScroll = scrollTop();
  const rects = rows.map(r => r.getBoundingClientRect());
  const mids = rects.map(r => r.top + r.height / 2);
  const h = rects[from].height;
  const startY = e.clientY;
  let lastY = e.clientY, to = from, raf = 0, done = false;
  const d = R.drag = { pending: false };
  li.classList.add('dragging');
  ul.classList.add('drag-active');
  try { grip.setPointerCapture(e.pointerId); } catch {}

  const layout = () => {
    const dy = lastY - startY + (scrollTop() - startScroll);
    li.style.transform = `translateY(${dy}px)`;
    const center = mids[from] + dy;
    to = from;
    for (let i = 0; i < rows.length; i++) {
      if (i < from && center < mids[i]) { to = i; break; }
    }
    for (let i = rows.length - 1; i > from; i--) {
      if (center > mids[i]) { to = i; break; }
    }
    rows.forEach((r, i) => {
      if (i === from) return;
      let shift = 0;
      if (from < to && i > from && i <= to) shift = -h;
      if (to < from && i >= to && i < from) shift = h;
      r.style.transform = shift ? `translateY(${shift}px)` : '';
    });
  };
  // Scroll the list when the finger nears the top or bottom edge.
  const edges = () => {
    if (wide) { const r = scroller.getBoundingClientRect(); return [Math.max(0, r.top), Math.min(window.innerHeight, r.bottom)]; }
    const headH = document.querySelector('.r-head').getBoundingClientRect().bottom;
    return [headH, window.innerHeight - 70]; // the tab bar covers the bottom on phones
  };
  const tick = () => {
    if (done) return;
    const [top, bottom] = edges();
    const zone = 60;
    let v = 0;
    if (lastY < top + zone) v = -Math.ceil((top + zone - lastY) / 6);
    else if (lastY > bottom - zone) v = Math.ceil((lastY - (bottom - zone)) / 6);
    if (v) { scroller.scrollTop += v; layout(); }
    raf = requestAnimationFrame(tick);
  };
  raf = requestAnimationFrame(tick);

  const move = ev => { lastY = ev.clientY; layout(); };
  const end = ev => {
    done = true;
    cancelAnimationFrame(raf);
    grip.removeEventListener('pointermove', move);
    grip.removeEventListener('pointerup', end);
    grip.removeEventListener('pointercancel', end);
    rows.forEach(r => { r.style.transform = ''; });
    li.classList.remove('dragging');
    ul.classList.remove('drag-active');
    R.drag = null;
    const cancelled = ev.type === 'pointercancel';
    if (!cancelled && to !== from) {
      const q = R.state.queue;
      const [t] = q.splice(from, 1);
      q.splice(to, 0, t); // show the new order right away; the server confirms it
      renderQueue();
      cmd('move', { uid: t.uid, to }).catch(() => api('/api/state').then(st => { st.status._rx = Date.now(); R.state = st; renderQueue(); }));
    } else if (d.pending) {
      renderQueue();
    }
  };
  grip.addEventListener('pointermove', move);
  grip.addEventListener('pointerup', end);
  grip.addEventListener('pointercancel', end);
}

// ---- Search ----------------------------------------------------------------
let searchSeq = 0;
async function runSearch(offset = 0) {
  const q = $('#search-q').value.trim();
  const box = $('#search-results');
  if (!q) { box.innerHTML = ''; return; }
  const seq = ++searchSeq;
  if (/^https?:\/\/|^spotify:/.test(q)) return resolveLink(q, box, seq);
  if (!offset) box.innerHTML = '<div class="spinner">Searching…</div>';
  try {
    const r = await api(`/api/search?source=${R.searchSrc}&q=${encodeURIComponent(q)}&offset=${offset}`);
    if (seq !== searchSeq) return;
    const prev = offset ? (box._tracks || []) : [];
    const tracks = prev.concat(r.items);
    box._tracks = tracks;
    box.innerHTML = tracks.length ? `<ul class="list">${tracks.map((t, i) => trackRow(t, { i })).join('')}</ul>` +
      (r.next != null ? `<div class="row-actions"><button class="ghost" id="more">Load more</button></div>` : '') :
      '<div class="empty-note">No results</div>';
    bindTrackList(box, tracks, browseMenu);
    const more = $('#more');
    if (more) more.onclick = () => { more.disabled = true; runSearch(r.next); };
  } catch (e) {
    if (seq === searchSeq) box.innerHTML = `<div class="empty-note">${esc(e.message)}</div>`;
  }
}

async function resolveLink(url, box, seq) {
  box.innerHTML = '<div class="spinner">Opening link…</div>';
  try {
    const r = await api('/api/resolve', { method: 'POST', body: { url } });
    if (seq !== searchSeq) return;
    if (r.kind === 'user') {
      box.innerHTML = `<div class="pl" style="cursor:default">${art(r.user.avatar)}<div class="m"><div class="t">${esc(r.user.username)}</div><div class="muted small">SoundCloud profile</div></div></div>
        <div class="row-actions"><button class="primary" id="use-profile">Use as my SoundCloud library</button></div>`;
      $('#use-profile').onclick = async () => {
        try {
          R.info = await api('/api/settings', { method: 'POST', body: { scProfile: r.user.url || url } });
          R.libSrc = 'soundcloud'; R.libStack = [];
          document.querySelectorAll('#lib-src button').forEach(x => x.classList.toggle('on', x.dataset.src === 'soundcloud'));
          toast('Library set to ' + r.user.username);
          show('library');
        } catch (e) { toast(e.message, true); }
      };
      return;
    }
    showTrackCollection(box, r.name || (r.kind === 'track' ? '' : 'Link'), r.tracks);
  } catch (e) {
    if (seq === searchSeq) box.innerHTML = `<div class="empty-note">${esc(e.message)}</div>`;
  }
}

// A named list of tracks with bulk actions (used for playlists, likes, links).
function showTrackCollection(box, name, tracks, extraActions = [], menuFor = browseMenu) {
  const playable = tracks.filter(t => !['drm', 'unavailable'].includes(t.playable));
  box.innerHTML = (name ? `<h2>${esc(name)}</h2>` : '') +
    `<div class="row-actions">
      <button class="primary" data-b="play"><span class="ic">${ICON.play}</span>Play all</button>
      <button class="ghost" data-b="add">Add all to queue</button>
      <button class="ghost" data-b="save">Save as playlist</button>
      ${extraActions.map((a, i) => `<button class="ghost" data-x="${i}">${esc(a[0])}</button>`).join('')}
      <span class="muted small" style="align-self:center">${tracks.length} tracks</span></div>` +
    `<ul class="list">${tracks.map((t, i) => trackRow(t, { i })).join('')}</ul>`;
  bindTrackList(box, tracks, menuFor);
  box.querySelector('[data-b=play]').onclick = async () => { if (!playable.length) return; await cmd('replace', { tracks: playable, play: true }); toast('Playing ' + (name || 'tracks')); };
  box.querySelector('[data-b=add]').onclick = () => addTracks(playable, 'end');
  box.querySelector('[data-b=save]').onclick = async () => {
    const n = prompt('Playlist name', name || 'New playlist');
    if (!n) return;
    await api('/api/playlists', { method: 'POST', body: { name: n, tracks: playable } });
    toast('Saved “' + n + '”');
  };
  box.querySelectorAll('[data-x]').forEach(b => b.onclick = () => extraActions[Number(b.dataset.x)][1]());
}

// ---- Library ---------------------------------------------------------------
async function renderLibrary() {
  const box = $('#library');
  renderLibAccount();
  const top = R.libStack[R.libStack.length - 1];
  const back = R.libStack.length ? `<button class="back" id="lib-back">‹ Back</button>` : '';
  if (top) {
    box.innerHTML = back + '<div class="spinner">Loading…</div>';
    $('#lib-back').onclick = () => { R.libStack.pop(); renderLibrary(); };
    try {
      const data = await top.load();
      const inner = document.createElement('div');
      showTrackCollection(inner, data.name, data.tracks);
      box.innerHTML = back;
      box.appendChild(inner);
      if (data.more) {
        const m = document.createElement('div');
        m.className = 'row-actions';
        m.innerHTML = '<button class="ghost">Load more</button>';
        m.firstChild.onclick = async () => { m.firstChild.disabled = true; await data.more(); renderLibrary(); };
        box.appendChild(m);
      }
      $('#lib-back').onclick = () => { R.libStack.pop(); renderLibrary(); };
    } catch (e) { box.innerHTML = back + `<div class="empty-note">${esc(e.message)}</div>`; $('#lib-back').onclick = () => { R.libStack.pop(); renderLibrary(); }; }
    return;
  }
  box.innerHTML = '<div class="spinner">Loading…</div>';
  try {
    if (R.libSrc === 'spotify') {
      if (R.info && !R.info.spotify.loggedIn) { box.innerHTML = `<div class="empty-note">Spotify isn't connected yet. <a href="#settings">Set it up in Settings</a>.</div>`; return; }
      const pls = await api('/api/library/spotify/playlists');
      box.innerHTML = `<div class="pl" data-k="liked"><div class="art" style="background:linear-gradient(135deg,#4b2bd1,#8fd3c3)">♥</div><div class="m"><div class="t">Liked Songs</div><div class="muted small">Spotify</div></div></div>` +
        pls.map((p, i) => `<div class="pl ${p.readable ? '' : 'dim'}" data-i="${i}">${art(p.artwork)}<div class="m"><div class="t">${esc(p.name)}</div>
          <div class="muted small">${p.readable ? p.count + ' tracks' : 'Spotify only shares playlists you own or collaborate on'}</div></div></div>`).join('');
      box.querySelector('[data-k=liked]').onclick = () => pushLib(likedLoader('spotify'));
      box.querySelectorAll('.pl[data-i]').forEach(el => el.onclick = () => {
        const p = pls[Number(el.dataset.i)];
        if (!p.readable) return toast("Spotify doesn't let apps read playlists you don't own. Copy it into your own playlist in Spotify first.", true);
        pushLib({ load: () => api('/api/library/spotify/playlist/' + p.id) });
      });
    } else {
      if (R.info && !R.info.soundcloud.username && !R.info.soundcloud.hasToken) { box.innerHTML = `<div class="empty-note">Paste your SoundCloud profile link in <a href="#settings">Settings</a> (or in Search) to see your likes and playlists. Search works without it.</div>`; return; }
      const pls = await api('/api/library/soundcloud/playlists');
      box.innerHTML = (R.info && R.info.soundcloud.profile ? `<p class="muted small">Showing <b>${esc(R.info.soundcloud.profile.username)}</b> · <a href="#settings">change</a></p>` : '') + `<div class="pl" data-k="liked"><div class="art" style="background:linear-gradient(135deg,#ff7a1a,#ff3d6e)">♥</div><div class="m"><div class="t">Likes</div><div class="muted small">SoundCloud</div></div></div>` +
        pls.map((p, i) => `<div class="pl" data-i="${i}">${art(p.artwork)}<div class="m"><div class="t">${esc(p.name)}</div><div class="muted small">${p.count} tracks${p.owner ? ' · ' + esc(p.owner) : ''}${p.liked ? ' · ♥ liked' : ''}</div></div></div>`).join('') +
        (pls.length ? '' : '<div class="empty-note">No playlists found on this profile.</div>');
      box.querySelector('[data-k=liked]').onclick = () => pushLib(likedLoader('soundcloud'));
      box.querySelectorAll('.pl[data-i]').forEach(el => el.onclick = () => {
        const p = pls[Number(el.dataset.i)];
        pushLib({ load: () => api('/api/library/soundcloud/playlist/' + p.id) });
      });
    }
  } catch (e) { box.innerHTML = `<div class="empty-note">${esc(e.message)}</div>`; }
}
function pushLib(entry) { R.libStack.push(entry); renderLibrary(); window.scrollTo(0, 0); }

// Liked tracks load in pages; keep what's loaded between "Load more" clicks.
function likedLoader(src) {
  const acc = { tracks: [], cursor: null, done: false, started: false };
  const fetchPage = async () => {
    const url = src === 'spotify' ? `/api/library/spotify/liked?offset=${acc.cursor || 0}` : `/api/library/soundcloud/likes${acc.cursor ? '?next=' + encodeURIComponent(acc.cursor) : ''}`;
    const r = await api(url);
    acc.tracks.push(...r.items);
    acc.cursor = r.next;
    acc.done = r.next == null;
  };
  return {
    load: async () => {
      if (!acc.started) { acc.started = true; await fetchPage(); }
      return { name: src === 'spotify' ? 'Liked Songs' : 'SoundCloud Likes', tracks: acc.tracks, more: acc.done ? null : fetchPage };
    },
  };
}

// ---- Saved (MixDeck) playlists ----------------------------------------------
async function renderSaved() {
  const box = $('#saved');
  const top = R.savedStack[R.savedStack.length - 1];
  if (top) {
    box.innerHTML = '<button class="back">‹ Back</button><div class="spinner">Loading…</div>';
    box.querySelector('.back').onclick = () => { R.savedStack.pop(); renderSaved(); };
    try {
      const p = await api('/api/playlists/' + top);
      const inner = document.createElement('div');
      // Tracks inside a saved playlist also get a "remove from playlist" option.
      const menu = t => [...browseMenu(t), ['Remove from this playlist', async () => {
        p.tracks.splice(p.tracks.indexOf(t), 1);
        await api('/api/playlists/' + p.id, { method: 'PUT', body: { tracks: p.tracks } });
        renderSaved();
      }, 'danger']];
      showTrackCollection(inner, p.name, p.tracks, [
        ['Replace with current queue', async () => { if (!confirm(`Replace "${p.name}" with the current queue?`)) return; await api('/api/playlists/' + p.id, { method: 'PUT', body: { fromQueue: true } }); toast('Updated'); renderSaved(); }],
        ['Rename', async () => { const n = prompt('New name', p.name); if (!n) return; await api('/api/playlists/' + p.id, { method: 'PUT', body: { name: n } }); renderSaved(); }],
        ['Delete', async () => { if (!confirm(`Delete "${p.name}"?`)) return; await api('/api/playlists/' + p.id, { method: 'DELETE' }); R.savedStack.pop(); renderSaved(); }],
      ], menu);
      inner.querySelector('[data-b=save]').remove();
      box.innerHTML = '<button class="back">‹ Back</button>';
      box.querySelector('.back').onclick = () => { R.savedStack.pop(); renderSaved(); };
      box.appendChild(inner);
    } catch (e) { box.innerHTML = `<div class="empty-note">${esc(e.message)}</div>`; }
    return;
  }
  box.innerHTML = '<div class="spinner">Loading…</div>';
  const list = await api('/api/playlists').catch(e => { box.innerHTML = `<div class="empty-note">${esc(e.message)}</div>`; return null; });
  if (!list) return;
  box.innerHTML = `<h2>Your MixDeck playlists</h2><p class="muted small">These mix Spotify and SoundCloud tracks and are saved on the host computer.</p>
    <div class="row-actions"><button class="primary" id="save-q">Save current queue</button></div>` +
    (list.length ? list.map(p => `<div class="pl" data-id="${p.id}"><div class="art">★</div><div class="m"><div class="t">${esc(p.name)}</div><div class="muted small">${p.count} tracks</div></div></div>`).join('')
      : '<div class="empty-note">No saved playlists yet. Build a queue, then save it here.</div>');
  $('#save-q').onclick = saveQueue;
  box.querySelectorAll('.pl').forEach(el => el.onclick = () => { R.savedStack.push(el.dataset.id); renderSaved(); });
}

async function saveQueue() {
  if (!R.state.queue.length) return toast('The queue is empty', true);
  const n = prompt('Name this playlist', 'Mix ' + new Date().toLocaleDateString());
  if (!n) return;
  await api('/api/playlists', { method: 'POST', body: { name: n, fromQueue: true } });
  toast('Saved “' + n + '”');
  if (R.view === 'saved') renderSaved();
}

async function pickSavedPlaylist(tracks) {
  const list = await api('/api/playlists');
  openSheet('Add to playlist', [
    ['+ New playlist…', async () => {
      const n = prompt('Playlist name');
      if (!n) return;
      await api('/api/playlists', { method: 'POST', body: { name: n, tracks } });
      toast('Saved to “' + n + '”');
    }],
    ...list.map(p => [p.name, async () => {
      await api('/api/playlists/' + p.id, { method: 'PUT', body: { append: tracks } });
      toast('Added to “' + p.name + '”');
    }]),
  ]);
}

// ---- Settings ----------------------------------------------------------------
// ---- Linked accounts ------------------------------------------------------------
function accountsOf(service) { return (R.info && R.info[service] && R.info[service].accounts) || []; }
function activeAccountKey() { return ['spotify', 'soundcloud'].map(s => (accountsOf(s).find(a => a.active) || {}).id || '').join('|'); }
async function switchAccount(service, id) {
  try {
    R.info = await api('/api/accounts/switch', { method: 'POST', body: { service, id } });
    R.acctKey = activeAccountKey();
    R.libStack = [];
    const a = accountsOf(service).find(x => x.active);
    toast('Switched to ' + (a ? a.name : 'account'));
  } catch (e) { toast(e.message, true); }
}
function accountList(service) {
  const list = accountsOf(service);
  if (!list.length) return '';
  const many = list.length > 1;
  return `<div class="acct-list">${list.map(a => `<div class="acct ${a.active ? 'on' : ''}">
      <div class="av" ${a.avatar ? `style="background-image:url('${esc(a.avatar)}')"` : ''}>${a.avatar ? '' : esc((a.name || '?').slice(0, 1).toUpperCase())}</div>
      <div class="n">${esc(a.name)}${a.hasToken ? ' <span class="pill ok small">private</span>' : ''}</div>
      ${a.active ? (many ? '<span class="pill ok small">Active</span>' : '') : `<button class="ghost" data-acct-switch="${service}" data-id="${esc(a.id)}">Switch</button>`}
      <button class="ghost danger" data-acct-remove="${service}" data-id="${esc(a.id)}" ${IS_HOST ? '' : 'disabled'} title="Unlink this account">Remove</button>
    </div>`).join('')}</div>`;
}
function bindAccountButtons(root, rerender) {
  root.querySelectorAll('[data-acct-switch]').forEach(b => b.onclick = async () => { await switchAccount(b.dataset.acctSwitch, b.dataset.id); rerender(); });
  root.querySelectorAll('[data-acct-remove]').forEach(b => b.onclick = async () => {
    const a = accountsOf(b.dataset.acctRemove).find(x => x.id === b.dataset.id);
    if (!confirm(`Unlink ${a ? a.name : 'this account'} from MixDeck?`)) return;
    try { R.info = await api('/api/accounts/remove', { method: 'POST', body: { service: b.dataset.acctRemove, id: b.dataset.id } }); R.acctKey = activeAccountKey(); R.libStack = []; toast('Removed'); } catch (e) { toast(e.message, true); }
    rerender();
  });
}

// Account picker above the library when a service has more than one linked account.
function renderLibAccount() {
  const el = $('#lib-acct');
  const list = accountsOf(R.libSrc);
  if (list.length < 2 || R.libStack.length) { el.innerHTML = ''; return; }
  el.innerHTML = `Account <select id="lib-acct-sel">${list.map(a => `<option value="${esc(a.id)}" ${a.active ? 'selected' : ''}>${esc(a.name)}</option>`).join('')}</select>`;
  $('#lib-acct-sel').onchange = async e => { await switchAccount(R.libSrc, e.target.value); renderLibrary(); };
}

async function renderSettings() {
  const info = R.info = await api('/api/info');
  const s = info.settings;
  const hostOnly = IS_HOST ? '' : 'disabled';
  const hostNote = IS_HOST ? '' : `<p class="pill wait">Open http://127.0.0.1:${info.port}/ on the host computer to change this.</p>`;
  const sp = info.spotify, scs = info.soundcloud;
  const box = $('#settings');
  box.innerHTML = `
  ${info.app ? `<div class="set">
    <h3>Player</h3>
    <p>${info.playerOnline ? '<span class="pill ok">Player is running</span>' : '<span class="pill bad">Player is not running</span>'}
      ${R.state.status && R.state.status.host === 'browser' ? `<span class="muted small">in ${esc(info.defaultBrowser || 'your browser')}</span>` : ''}</p>
    <label>Where the player runs</label>
    <select id="player-host" ${hostOnly}>
      <option value="auto" ${s.playerHost === 'auto' ? 'selected' : ''}>Automatic: inside MixDeck, browser if Spotify needs it</option>
      <option value="app" ${s.playerHost === 'app' ? 'selected' : ''}>Always inside MixDeck</option>
      <option value="browser" ${s.playerHost === 'browser' ? 'selected' : ''}>Always in ${esc(info.defaultBrowser || (info.platform === 'android' ? 'Chrome' : 'my default browser'))}</option>
    </select>
    <p>${info.platform === 'android'
      ? "Spotify's player needs DRM, which doesn't always work inside apps. In Automatic mode, if Spotify can't play inside MixDeck, the player moves to Chrome. Keep that Chrome tab open while you listen."
      : `Spotify's player needs browser DRM, which doesn't always work inside the MixDeck window. In Automatic mode, MixDeck moves the player to ${esc(info.defaultBrowser || 'your default browser')} the first time that happens. It opens as a separate small window with its own profile, so your normal browsing isn't affected.`}</p>
    <div class="row-actions"><button class="ghost" id="player-restart" ${hostOnly}>Restart player</button></div>
    <div id="join-qr" class="join" style="margin-top:10px"></div>
  </div>

  <div class="set">
    <h3>MixDeck app</h3>
    ${info.platform === 'android' ? `<p>Music keeps playing with the screen off. MixDeck shows a notification while it's running; swipe MixDeck away from your recent apps to stop it completely.</p>` : `
    <label class="switch"><input type="checkbox" id="autostart" ${s.startWithWindows ? 'checked' : ''} ${hostOnly}> Start MixDeck when ${info.platform === 'linux' ? 'I log in' : 'Windows starts'}</label>
    <label class="switch"><input type="checkbox" id="closetray" ${s.closeToTray ? 'checked' : ''} ${hostOnly}> Keep playing in the tray when I close the window</label>
    ${info.platform === 'linux' ? '<p>Some Linux desktops (like GNOME) hide tray icons unless you add an AppIndicator extension. Only turn on the tray option if you can see MixDeck\'s icon.</p>' : ''}`}
    <p>Your settings, playlists and downloads are in <code>${esc(info.dataDir)}</code>. Version ${esc(info.version)}.</p>
    <div class="row-actions">
      <button class="ghost" id="rerun-setup" ${hostOnly}>Run setup again</button>
      ${info.platform === 'android' ? '' : `<button class="ghost" id="import-old" ${hostOnly}>Import from the old MixDeck folder</button>`}
      <button class="ghost" id="show-log">View log</button>
    </div>
  </div>
` : `<div class="set">
    <h3>Host player</h3>
    <p>${info.playerOnline ? '<span class="pill ok">Player is open</span>' : '<span class="pill bad">Player is not open</span>'}</p>
    <p>Music plays from the computer running MixDeck, in the tab at <code>http://127.0.0.1:${info.port}/player</code>.</p>
    ${IS_HOST ? `<p><a class="btn primary" href="/player" target="_blank">Open host player</a></p>` : ''}
    <div id="join-qr" class="join" style="margin-top:10px"></div>
  </div>
`}
  <div class="set">
    <h3>Playback</h3>
    <label>Crossfade: <b id="xf-val">${s.crossfade}s</b></label>
    <input type="range" id="xf" min="0" max="15" step="0.5" value="${s.crossfade}" style="width:100%">
    <label class="switch"><input type="checkbox" id="spsafe" ${s.spotifySafe ? 'checked' : ''}> Spotify-safe transitions</label>
    <p>Spotify's Developer Policy doesn't allow Spotify tracks to overlap other audio. When this is on, transitions that involve a Spotify track don't overlap; SoundCloud-to-SoundCloud transitions still crossfade.</p>
    <label>Download this many upcoming SoundCloud/YouTube songs ahead</label>
    <input type="number" id="ahead" min="1" max="5" value="${s.preloadAhead || 2}" style="max-width:100px">
  </div>

  <div class="set">
    <h3>Spotify</h3>
    <p>${sp.loggedIn ? `<span class="pill ok">Logged in${sp.user ? ' as ' + esc(sp.user.name) : ''}</span>` : '<span class="pill bad">Not connected</span>'}</p>
    ${accountList('spotify')}
    ${hostNote}
    <label>Client ID (from developer.spotify.com/dashboard)</label>
    <div class="inline"><input type="text" id="sp-cid" value="${esc(sp.clientId)}" placeholder="32-character Client ID" ${hostOnly}><button class="ghost" id="sp-cid-save" ${hostOnly}>Save</button></div>
    <p>Redirect URI to add in your Spotify app: <code>${esc(sp.redirectUri)}</code></p>
    <div class="row-actions">
      ${sp.loggedIn ? `<button class="ghost" id="sp-login" ${IS_HOST && sp.clientId ? '' : 'disabled'}>Add another account</button>` : `<button class="primary" id="sp-login" ${IS_HOST && sp.clientId ? '' : 'disabled'}>Log in with Spotify</button>`}
    </div>
    <p class="small">Needs Spotify Premium. In the dashboard, tick "Web API" and "Web Playback SDK", and add your Spotify email under User Management.${sp.loggedIn ? ' To add another account, add its email under User Management too (each needs Premium), then use Add another account and choose "Not you?" on Spotify\'s page to log in as them.' : ''}</p>
  </div>

  <div class="set">
    <h3>SoundCloud</h3>
    ${accountsOf('soundcloud').length > 1 ? accountList('soundcloud') : ''}
    <label>${accountsOf('soundcloud').length ? 'Add another SoundCloud account (profile link)' : 'Your SoundCloud profile link'}</label>
    <div class="inline"><input type="text" id="sc-user" value="${accountsOf('soundcloud').length ? '' : esc(scs.profile ? scs.profile.url : scs.username)}" placeholder="https://soundcloud.com/yourname" autocapitalize="off" autocorrect="off" spellcheck="false"><button class="ghost" id="sc-user-save">${accountsOf('soundcloud').length ? 'Add' : 'Save'}</button></div>
    <div id="sc-user-msg"></div>
    ${scs.profile && accountsOf('soundcloud').length < 2 ? `<div class="pl" style="cursor:default;margin-top:8px">${art(scs.profile.avatar)}<div class="m"><div class="t">${esc(scs.profile.username)}</div>
      <div class="muted small">soundcloud.com/${esc(scs.profile.permalink)}${scs.profile.likes != null ? ` · ${scs.profile.likes} likes` : ''}${scs.profile.playlists != null ? ` · ${scs.profile.playlists} playlists` : ''}</div></div>
      <button class="ghost" id="sc-user-clear">Remove</button></div>` : ''}
    <p>On soundcloud.com, open your profile and copy the address bar. In the SoundCloud app, go to your profile, tap Share → Copy link. A username works too. Search and links work without this. Only public likes and playlists show up unless you add the token below.</p>
    <details ${hostOnly ? '' : ''}><summary class="muted small" style="cursor:pointer;margin-top:10px">Advanced: private likes, client ID</summary>
      ${hostNote}
      <label>OAuth token (optional, shows private likes/playlists${accountsOf('soundcloud').length > 1 ? ' for the active account' : ''}) ${scs.hasToken ? '<span class="pill ok">set</span>' : ''}</label>
      <div class="inline"><input type="password" id="sc-token" placeholder="${scs.hasToken ? '•••••• (saved)' : 'value of the oauth_token cookie'}" ${hostOnly}><button class="ghost" id="sc-token-save" ${hostOnly}>Save</button></div>
      <p>Log in at soundcloud.com in your browser, open DevTools → Application → Cookies → soundcloud.com and copy <code>oauth_token</code>. It stays on this computer. Leave blank to remove.</p>
      <label>client_id override (normally found automatically)</label>
      <div class="inline"><input type="text" id="sc-cid" value="${esc(scs.manualClientId)}" placeholder="leave blank for automatic" ${hostOnly}><button class="ghost" id="sc-cid-save" ${hostOnly}>Save</button></div>
      <div class="row-actions"><button class="ghost" id="sc-refresh">Re-detect client_id now</button></div>
    </details>
  </div>

  <div class="set">
    <h3>YouTube</h3>
    <p>Search YouTube or paste a video or playlist link.${info.platform === 'android' ? '' : ' Audio is fetched with <b>yt-dlp</b>, which MixDeck downloads the first time you use YouTube and keeps in the data folder.'}</p>
    <p id="yt-status" class="muted small">Checking…</p>
    <div class="row-actions"><button class="ghost" id="yt-update" ${hostOnly}>Update yt-dlp</button></div>
    <p class="small">If YouTube songs stop loading, update yt-dlp first. YouTube changes often, and yt-dlp releases fixes within days.</p>
  </div>

  <div class="set">
    <h3>Download cache</h3>
    <p>SoundCloud and YouTube songs are saved as they play and download ahead of time, so crossfades are seamless and replays are instant. Using <b id="cache-size">…</b>.</p>
    <label>Maximum size (MB)</label>
    <div class="inline"><input type="number" id="cache-mb" min="100" step="100" value="${s.cacheMB}" style="max-width:140px"><button class="ghost" id="cache-mb-save">Save</button><button class="ghost danger" id="cache-clear">Clear cache</button></div>
  </div>`;

  api('/api/cache').then(c => { $('#cache-size').textContent = (c.bytes / 1048576).toFixed(0) + ' MB'; }).catch(() => {});
  api('/api/youtube/status').then(y => {
    const el = $('#yt-status'); if (!el) return;
    if (y.builtin) {
      el.innerHTML = '<span class="pill ok">Built in</span> YouTube support is part of this version of MixDeck.';
      const b = $('#yt-update'); if (b) b.style.display = 'none';
      return;
    }
    el.innerHTML = (y.ytdlp && y.ytdlp.installed ? `<span class="pill ok">yt-dlp ${esc(y.ytdlp.version || '')}</span>` : '<span class="pill wait">yt-dlp will download on first use</span>') +
      (y.js ? ` <span class="pill ok">${y.js.kind === 'node' ? 'Node.js' : 'Deno'}</span>` : ' <span class="pill wait">a JavaScript runtime (Deno) will download on first use</span>');
  }).catch(e => { const el = $('#yt-status'); if (el) el.textContent = e.message; });
  const url = info.lanUrls[0];
  if (url && window.qrcode) {
    const qr = qrcode(0, 'M'); qr.addData(url); qr.make();
    $('#join-qr').innerHTML = `<div class="qr">${qr.createSvgTag({ cellSize: 4, margin: 2, scalable: true })}</div><div><div class="lbl">Open on your phone</div><div class="url">${info.lanUrls.map(esc).join('<br>')}</div><div class="muted small">Phone must be on the same Wi-Fi.</div></div>`;
  } else $('#join-qr').remove();

  const save = async (body, msg = 'Saved') => { try { R.info = await api('/api/settings', { method: 'POST', body }); toast(msg); } catch (e) { toast(e.message, true); } };
  const xf = $('#xf');
  xf.oninput = () => { $('#xf-val').textContent = xf.value + 's'; };
  xf.onchange = () => save({ crossfade: Number(xf.value) });
  $('#spsafe').onchange = e => save({ spotifySafe: e.target.checked });
  $('#ahead').onchange = e => save({ preloadAhead: Number(e.target.value) });
  $('#sp-cid-save').onclick = async () => { await save({ spotifyClientId: $('#sp-cid').value }); renderSettings(); };
  const li = $('#sp-login'); if (li) li.onclick = () => { if (info.app) { openExternal('/auth/spotify/login'); toast('Finish logging in, in your browser'); } else location.href = '/auth/spotify/login'; };
  const ph = $('#player-host'); if (ph) ph.onchange = async () => { await save({ playerHost: ph.value }, 'Player moved'); setTimeout(renderSettings, 1500); };
  const pr = $('#player-restart'); if (pr) pr.onclick = async () => { await api('/api/player/restart', { method: 'POST' }).catch(e => toast(e.message, true)); toast('Restarting the player'); setTimeout(renderSettings, 2000); };
  const as = $('#autostart'); if (as) as.onchange = () => save({ startWithWindows: as.checked });
  const ct = $('#closetray'); if (ct) ct.onchange = () => save({ closeToTray: ct.checked });
  const rs = $('#rerun-setup'); if (rs) rs.onclick = () => { location.href = '/setup'; };
  const io = $('#import-old'); if (io) io.onclick = importOld;
  const sl = $('#show-log'); if (sl) sl.onclick = showLog;
  bindAccountButtons(box, renderSettings);
  const saveProfile = async value => {
    const btn = $('#sc-user-save'), msg = $('#sc-user-msg');
    btn.disabled = true; msg.innerHTML = value ? '<p>Looking up profile…</p>' : '';
    try {
      const adding = !!value && accountsOf('soundcloud').length > 0;
      R.info = await api('/api/settings', { method: 'POST', body: { scProfile: value, scAddAccount: adding } });
      R.acctKey = activeAccountKey();
      R.libStack = [];
      toast(value ? (adding ? 'Added ' : 'Found ') + R.info.soundcloud.profile.username : 'Removed');
      renderSettings();
    } catch (e) {
      msg.innerHTML = `<p class="pill bad" style="display:block;border-radius:8px">${esc(e.message)}</p>`;
      btn.disabled = false;
    }
  };
  $('#sc-user-save').onclick = () => saveProfile($('#sc-user').value.trim());
  $('#sc-user').addEventListener('keydown', e => { if (e.key === 'Enter') saveProfile($('#sc-user').value.trim()); });
  const clr = $('#sc-user-clear'); if (clr) clr.onclick = async () => {
    const a = accountsOf('soundcloud').find(x => x.active);
    try { R.info = await api('/api/accounts/remove', { method: 'POST', body: { service: 'soundcloud', id: a ? a.id : '' } }); R.acctKey = activeAccountKey(); R.libStack = []; toast('Removed'); } catch (e) { toast(e.message, true); }
    renderSettings();
  };
  $('#sc-token-save').onclick = async () => { await save({ scOauthToken: $('#sc-token').value }); renderSettings(); };
  $('#sc-cid-save').onclick = async () => { await save({ scClientId: $('#sc-cid').value }); renderSettings(); };
  $('#sc-refresh').onclick = async () => { try { const r = await api('/api/soundcloud/refresh', { method: 'POST' }); toast('Found client_id ' + r.clientId); } catch (e) { toast(e.message, true); } };
  $('#yt-update').onclick = async () => {
    const b = $('#yt-update'); b.disabled = true; b.textContent = 'Updating…';
    try { const r = await api('/api/youtube/update', { method: 'POST' }); toast(r.output || 'Updated'); } catch (e) { toast(e.message, true); }
    renderSettings();
  };
  $('#cache-mb-save').onclick = () => save({ cacheMB: Number($('#cache-mb').value) });
  $('#cache-clear').onclick = async () => { await api('/api/cache/clear', { method: 'POST' }); toast('Cache cleared'); renderSettings(); };
}

async function importOld() {
  try {
    const r = await api('/api/import', { method: 'POST' });
    if (r.cancelled) return;
    toast(`Imported ${[...r.imported, r.playlists ? r.playlists + ' playlists' : ''].filter(Boolean).join(', ') || 'nothing new'}`);
    renderSettings();
  } catch (e) { toast(e.message, true); }
}

async function showLog() {
  const r = await api('/api/logs');
  const sheet = $('#sheet');
  sheet.innerHTML = `<div class="sh-title"><div class="t">MixDeck log</div><div class="muted small">Most recent at the bottom</div></div>
    <pre class="logview">${esc(r.lines.join('\n')) || '(empty)'}</pre><button class="opt muted" data-close>Close</button>`;
  sheet.querySelector('[data-close]').onclick = closeSheet;
  $('#sheet-bg').classList.add('open');
  sheet.classList.add('open');
  const pre = sheet.querySelector('pre'); pre.scrollTop = pre.scrollHeight;
}

// ---------------------------------------------------------------------------
// Live updates
// ---------------------------------------------------------------------------
function fitQueueColumn() {
  const head = $('.r-head');
  if (head) document.documentElement.style.setProperty('--headh', Math.round(head.getBoundingClientRect().height) + 'px');
  const tabs = $('#tabs');
  if (tabs) document.documentElement.style.setProperty('--qtop', Math.round(tabs.getBoundingClientRect().bottom + window.scrollY) + 'px');
}
window.addEventListener('resize', fitQueueColumn);

function renderHost() {
  const st = R.state.status || {};
  const el = $('#hoststatus');
  el.className = 'pill ' + (st.online ? 'ok' : 'bad');
  el.textContent = st.online ? (st.playing ? 'Playing' : 'Player ready') : 'Player offline';
  const app = R.info && R.info.app;
  $('#banner').innerHTML = st.online || !R.connected ? '' :
    app ? (IS_HOST ? `The player isn't running. <a href="#" id="banner-restart">Restart player</a>` : `The player isn't running on the MixDeck computer. Restart it from MixDeck there.`)
    : (IS_HOST ? `The host player isn't open. <a href="/player" target="_blank">Open it</a> and click Start.` : `The host player isn't open. On the host computer, open http://127.0.0.1:${R.info ? R.info.port : 8787}/player and click Start.`);
  const br = $('#banner-restart');
  if (br) br.onclick = e => { e.preventDefault(); api('/api/player/restart', { method: 'POST' }).then(() => toast('Restarting the player')).catch(err => toast(err.message, true)); };
  fitQueueColumn();
}

// ---------------------------------------------------------------------------
// Android: this page hosts the audio engine itself in a hidden frame (on desktop the app
// keeps a separate hidden window for it).
// ---------------------------------------------------------------------------
function wantsEmbeddedPlayer() {
  const i = R.info;
  return !!(i && i.app && i.platform === 'android' && IS_HOST && i.settings.playerHost !== 'browser');
}
function syncEmbeddedPlayer(reload = false) {
  let f = document.getElementById('embedded-player');
  if (!wantsEmbeddedPlayer()) { if (f) f.remove(); return; }
  if (f && !reload) return;
  if (f) f.remove();
  f = document.createElement('iframe');
  f.id = 'embedded-player';
  f.src = '/player?auto=1&host=app';
  f.allow = 'autoplay; encrypted-media';
  f.setAttribute('aria-hidden', 'true');
  f.style.cssText = 'position:fixed;width:1px;height:1px;left:-10px;top:-10px;border:0;opacity:0;pointer-events:none';
  document.body.appendChild(f);
}

function connect() {
  const es = new EventSource('/api/events');
  es.addEventListener('player-restart', () => syncEmbeddedPlayer(true));
  es.addEventListener('state', e => {
    const st = JSON.parse(e.data);
    st.status._rx = Date.now();
    R.state = st;
    renderNowPlaying(); renderQueue(); renderHost();
  });
  es.addEventListener('status', e => {
    const prev = R.state.status || {};
    const st = JSON.parse(e.data);
    st._rx = Date.now();
    R.state.status = st;
    const structural = prev.playing !== st.playing || JSON.stringify(prev.next) !== JSON.stringify(st.next) || !!prev.transition !== !!st.transition || prev.message !== st.message || prev.duration !== st.duration || prev.online !== st.online;
    if (structural) { renderNowPlaying(); renderQueue(); }
    renderHost();
  });
  es.addEventListener('info', e => {
    R.info = JSON.parse(e.data);
    syncEmbeddedPlayer();
    // Account switched (here or from another device): the library belongs to the old one.
    const key = activeAccountKey();
    if (R.acctKey !== undefined && key !== R.acctKey) { R.libStack = []; if (R.view === 'library') renderLibrary(); }
    R.acctKey = key;
    if (R.view === 'settings') renderSettings();
  });
  es.onopen = () => { R.connected = true; renderHost(); };
  es.onerror = () => { R.connected = false; const el = $('#hoststatus'); el.className = 'pill bad'; el.textContent = 'Server unreachable'; };
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------
document.querySelectorAll('#tabs button').forEach(b => b.onclick = () => {
  if (b.dataset.view === R.view) { if (b.dataset.view === 'library') { R.libStack = []; renderLibrary(); } if (b.dataset.view === 'saved') { R.savedStack = []; renderSaved(); } }
  show(b.dataset.view);
});
document.querySelectorAll('#search-src button').forEach(b => b.onclick = () => {
  R.searchSrc = b.dataset.src;
  document.querySelectorAll('#search-src button').forEach(x => x.classList.toggle('on', x === b));
  runSearch();
});
document.querySelectorAll('#lib-src button').forEach(b => b.onclick = () => {
  R.libSrc = b.dataset.src; R.libStack = [];
  document.querySelectorAll('#lib-src button').forEach(x => x.classList.toggle('on', x === b));
  renderLibrary();
});
let searchTimer;
$('#search-form').onsubmit = e => { e.preventDefault(); clearTimeout(searchTimer); runSearch(); $('#search-q').blur(); };
$('#search-q').addEventListener('input', () => { clearTimeout(searchTimer); searchTimer = setTimeout(() => runSearch(), R.searchSrc === 'youtube' ? 900 : 450); }); // YouTube searches are slower, so wait for typing to settle
$('#sheet-bg').onclick = closeSheet;
$('#q-save').onclick = saveQueue;
$('#q-shuffle').onclick = () => cmd('shuffle').then(() => toast('Shuffled'));
$('#q-clear').onclick = () => { if (confirm('Clear the queue? The current track keeps playing.')) cmd('clear', { keepCurrent: true }); };
window.addEventListener('hashchange', () => { const v = location.hash.slice(1); if (v && v !== R.view) show(v); });
window.addEventListener('resize', () => { if ((R.view === 'queue' && WIDE()) || (R.view === 'nowplaying' && !WIDE())) show(R.view); });

api('/api/info').then(i => { R.info = i; R.acctKey = activeAccountKey(); renderHost(); syncEmbeddedPlayer(); if (R.view === 'library') renderLibAccount(); }).catch(() => {});
connect();
setInterval(tickProgress, 500);
show(location.hash.slice(1) || (WIDE() ? 'search' : 'queue'));

// MixDeck host player: the only page that makes sound.
//
// Decks
//   scA, scB  – two stream decks for SoundCloud and YouTube (audio element -> hls.js when
//               needed -> Web Audio gain) so one can play while the other is preloaded.
//   sp        – the Spotify Web Playback SDK device. One Spotify account can only play one
//               stream at a time, so Spotify -> Spotify is fade-out then fade-in.
//
// The engine watches the current deck and starts the crossfade `crossfade` seconds before
// it ends. Everything that changes decks runs through one lock so commands never collide.
'use strict';

const $ = s => document.querySelector(s);
const PARAMS = new URLSearchParams(location.search);
// Where this player runs: 'app' (hidden window inside MixDeck), 'browser' (MixDeck opened
// it in the default browser) or 'tab' (opened by hand).
const HOST = PARAMS.get('host') || 'tab';
const AUTO = PARAMS.get('auto') === '1';

async function widevineAvailable() {
  if (!navigator.requestMediaKeySystemAccess) return false;
  try {
    await navigator.requestMediaKeySystemAccess('com.widevine.alpha', [{
      initDataTypes: ['cenc'],
      audioCapabilities: [{ contentType: 'audio/mp4; codecs="mp4a.40.2"', robustness: 'SW_SECURE_CRYPTO' }],
    }]);
    return true;
  } catch { return false; }
}

let drmReported = false;
// Spotify can't play here (no Widevine in this web view): ask MixDeck to move the player
// to the default browser. Only relevant for the in-app player.
function drmFailed(reason) {
  if (HOST !== 'app' || drmReported) return;
  drmReported = true;
  log('Moving the player to your browser (' + reason + ')', 'error');
  api('/api/player/drm-failed', { method: 'POST', body: { reason } }).catch(() => {});
}
// Spotify SDK promises occasionally never settle (e.g. when its connection hiccups). Never
// let one of those block the player: give each call a deadline.
const sleep = ms => new Promise(r => setTimeout(r, ms));
const within = (p, ms = 4000) => Promise.race([Promise.resolve(p).catch(() => {}), sleep(ms)]);
const ABORTED = Symbol('aborted');

const DRM_RE = /EME|DRM|key ?system|widevine|licen[cs]e|MediaKeys/i;
const S = { state: { queue: [], currentUid: null, settings: { crossfade: 8 } }, master: 1, failed: new Set(), log: [] };
let ctx, masterGain, scA, scB, sp;

function log(msg, kind = 'info') {
  console[kind === 'error' ? 'warn' : 'log']('[mixdeck]', msg);
  S.log.unshift({ t: new Date().toLocaleTimeString(), msg, kind });
  S.log.length = Math.min(S.log.length, 40);
  const el = $('#log');
  if (el) el.innerHTML = S.log.map(l => `<div class="${l.kind}"><span>${l.t}</span> ${esc(l.msg)}</div>`).join('');
}

// ---------------------------------------------------------------------------
// Equal-power fade curve (sounds even across the overlap, no dip in the middle)
// ---------------------------------------------------------------------------
function curve(from, to, p) {
  p = Math.max(0, Math.min(1, p));
  return to > from ? from + (to - from) * Math.sin(p * Math.PI / 2) : to + (from - to) * Math.cos(p * Math.PI / 2);
}

// ---------------------------------------------------------------------------
// SoundCloud deck
// ---------------------------------------------------------------------------
class ScDeck {
  constructor(name) {
    this.name = name;
    this.el = new Audio();
    this.el.preload = 'auto';
    this.node = ctx.createMediaElementSource(this.el);
    this.gain = ctx.createGain();
    this.gain.gain.value = 0;
    this.node.connect(this.gain).connect(masterGain);
    this.hls = null;
    this.track = null;
    this.level = 0;
    this.el.addEventListener('ended', () => engine.onEnded(this));
    this.el.addEventListener('error', () => { if (this.track) log(`Playback error on ${this.track.title}`, 'error'); });
  }
  get kind() { return 'soundcloud'; }

  async load(track) {
    this.unload();
    this.track = track;
    const token = this.token = Symbol();
    const prep = await api(`/api/prepare/${track.source}/${encodeURIComponent(track.id)}`, { method: 'POST' });
    if (token !== this.token) throw new Error('superseded');
    this.preview = prep.preview;
    this.partial = !!prep.partial; // still downloading: playback starts now, the rest streams in
    if (prep.type === 'hls' && window.Hls && Hls.isSupported()) {
      // Buffer the whole track: the file is already on local disk, so this is instant.
      // Segments that are still downloading make requests wait on the server, so be patient
      // with time-to-first-byte rather than treating it as a failure.
      const patient = {
        maxTimeToFirstByteMs: 90000, maxLoadTimeMs: 180000,
        timeoutRetry: { maxNumRetry: 3, retryDelayMs: 0, maxRetryDelayMs: 0 },
        errorRetry: { maxNumRetry: 6, retryDelayMs: 1000, maxRetryDelayMs: 8000 },
      };
      this.hls = new Hls({
        maxBufferLength: 3600, maxMaxBufferLength: 3600, maxBufferSize: 300 * 1000 * 1000, backBufferLength: 90, startPosition: 0,
        fragLoadPolicy: { default: patient },
      });
      this.hls.on(Hls.Events.ERROR, (e, d) => { if (d.fatal) log(`HLS error (${d.details}) on ${track.title}`, 'error'); });
      this.hls.loadSource(prep.src);
      this.hls.attachMedia(this.el);
    } else {
      this.el.src = prep.src;
    }
    await new Promise((resolve, reject) => {
      const done = () => { cleanup(); resolve(); };
      const fail = () => { cleanup(); reject(new Error('Could not decode audio')); };
      const hlsFail = (e, d) => { if (d.fatal) { cleanup(); reject(new Error('Browser could not play this stream (' + d.details + ')')); } };
      const timer = setTimeout(() => { cleanup(); reject(new Error('Timed out loading audio')); }, 30000);
      const cleanup = () => {
        clearTimeout(timer);
        this.el.removeEventListener('canplay', done); this.el.removeEventListener('error', fail);
        if (this.hls) this.hls.off(Hls.Events.ERROR, hlsFail);
      };
      if (this.el.readyState >= 3) return done();
      this.el.addEventListener('canplay', done);
      this.el.addEventListener('error', fail);
      if (this.hls) this.hls.on(Hls.Events.ERROR, hlsFail);
    });
    if (token !== this.token) throw new Error('superseded');
  }

  unload() {
    this.token = null;
    this.el.pause();
    if (this.hls) { this.hls.destroy(); this.hls = null; }
    this.el.removeAttribute('src');
    this.el.load();
    this.track = null;
    this.setLevel(0);
  }

  async play() { if (ctx.state !== 'running') await ctx.resume(); await this.el.play(); }
  pause() { this.el.pause(); }
  resume() { return this.play(); }
  stop() { this.unload(); }
  seek(ms) { this.el.currentTime = ms / 1000; }
  get position() { return this.el.currentTime * 1000; }
  get duration() { const d = this.el.duration; return isFinite(d) && d > 0 ? d * 1000 : (this.track ? this.track.duration : 0); }
  get playing() { return !this.el.paused && !this.el.ended; }

  setLevel(v) {
    const g = this.gain.gain;
    g.cancelScheduledValues(ctx.currentTime);
    g.setValueAtTime(v, ctx.currentTime);
    this.level = v;
  }
  // Sample-accurate fade scheduled on the audio clock (unaffected by background-tab timer throttling).
  fade(to, sec) {
    const from = this.level;
    this.level = to;
    const g = this.gain.gain;
    g.cancelScheduledValues(ctx.currentTime);
    if (sec <= 0.02) { g.setValueAtTime(to, ctx.currentTime); return { promise: Promise.resolve(), finish() {} }; }
    const pts = new Float32Array(128);
    for (let i = 0; i < pts.length; i++) pts[i] = curve(from, to, i / (pts.length - 1));
    g.setValueCurveAtTime(pts, ctx.currentTime, sec);
    let resolveFn, timer;
    const promise = new Promise(r => { resolveFn = r; timer = setTimeout(r, sec * 1000 + 30); });
    return { promise, finish: () => { clearTimeout(timer); g.cancelScheduledValues(ctx.currentTime); g.setValueAtTime(to, ctx.currentTime); resolveFn(); } };
  }
}

// ---------------------------------------------------------------------------
// Spotify deck (Web Playback SDK)
// ---------------------------------------------------------------------------
class SpotifyDeck {
  constructor() {
    this.player = null; this.deviceId = null; this.state = null; this.stateAt = 0;
    this.track = null; this.level = 1; this.ready = false; this.error = null;
    this._volTimer = null;
  }
  get kind() { return 'spotify'; }

  async init() {
    const tok = await api('/api/spotify/token').catch(() => null);
    if (!tok) { this.error = 'Not logged in to Spotify'; return; }
    this.initializing = true;
    if (!(await widevineAvailable())) {
      this.error = 'This window can\'t play Spotify (no DRM support)';
      drmFailed('Widevine unavailable');
      if (HOST === 'app') return;
    }
    await new Promise((resolve, reject) => {
      window.onSpotifyWebPlaybackSDKReady = resolve;
      const s = document.createElement('script');
      s.src = 'https://sdk.scdn.co/spotify-player.js';
      s.onerror = () => reject(new Error('Could not load the Spotify player (check your internet connection)'));
      document.head.appendChild(s);
    });
    this.player = new Spotify.Player({
      name: 'MixDeck',
      volume: 1,
      getOAuthToken: cb => api('/api/spotify/token').then(t => cb(t.token)).catch(e => log('Spotify token: ' + e.message, 'error')),
    });
    const p = this.player;
    p.addListener('ready', ({ device_id }) => { this.deviceId = device_id; this.ready = true; this.error = null; log('Spotify player ready'); render(); });
    p.addListener('not_ready', () => { this.ready = false; log('Spotify player went offline', 'error'); render(); });
    p.addListener('player_state_changed', st => { this.state = st; this.stateAt = performance.now(); });
    p.addListener('initialization_error', ({ message }) => { this.error = message; log('Spotify: ' + message, 'error'); if (DRM_RE.test(message)) drmFailed(message); render(); });
    p.addListener('authentication_error', ({ message }) => { this.error = 'Spotify login problem: ' + message; log(this.error, 'error'); render(); });
    p.addListener('account_error', ({ message }) => { this.error = 'Spotify Premium is required: ' + message; log(this.error, 'error'); render(); });
    p.addListener('playback_error', ({ message }) => { log('Spotify playback: ' + message, 'error'); if (DRM_RE.test(message)) drmFailed(message); });
    p.addListener('autoplay_failed', () => log('Browser blocked Spotify autoplay: click anywhere on this page', 'error'));
    if (p.activateElement) await p.activateElement();
    await p.connect();
    setInterval(async () => {
      if (!this.track || !this.player) return;
      const st = await within(this.player.getCurrentState(), 2000);
      if (st) { this.state = st; this.stateAt = performance.now(); }
    }, 400);
  }

  async webApi(path, opts = {}) {
    const { token } = await api('/api/spotify/token');
    const res = await fetch('https://api.spotify.com/v1' + path, { ...opts, signal: AbortSignal.timeout(10000), headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' } });
    if (!res.ok && res.status !== 204) {
      const j = await res.json().catch(() => ({}));
      throw new Error('Spotify: ' + ((j.error && j.error.message) || res.status));
    }
  }

  matches(track) {
    const ct = this.state && this.state.track_window && this.state.track_window.current_track;
    if (!ct || !track) return false;
    return ct.uri === track.uri || ct.id === track.id || (ct.linked_from && ct.linked_from.uri === track.uri) || ct.name === track.title;
  }

  async start(track, level) {
    if (!this.ready) throw new Error(this.error || 'Spotify player is not ready (log in from Settings)');
    this.track = track;
    await this.setLevel(level);
    await this.webApi(`/me/player/play?device_id=${this.deviceId}`, { method: 'PUT', body: JSON.stringify({ uris: [track.uri], position_ms: 0 }) });
    const t0 = performance.now();
    while (performance.now() - t0 < 10000) {
      if (this.matches(track) && !this.state.paused) return;
      await new Promise(r => setTimeout(r, 100));
    }
    throw new Error('Spotify did not start playing ' + track.title);
  }

  get position() {
    if (!this.state || !this.matches(this.track)) return 0;
    return this.state.paused ? this.state.position : this.state.position + (performance.now() - this.stateAt);
  }
  get duration() { return this.state && this.matches(this.track) ? this.state.duration : (this.track ? this.track.duration : 0); }
  get playing() { return !!(this.state && this.track && !this.state.paused && this.matches(this.track)); }

  setLevel(v) { this.level = v; return this.player ? within(this.player.setVolume(Math.max(0, Math.min(1, v * S.master))), 2000) : Promise.resolve(); }
  applyMaster() { return this.setLevel(this.level); }

  // The SDK has no scheduled ramps, so step the volume ~16x per second.
  fade(to, sec) {
    const from = this.level;
    if (sec <= 0.02) { this.setLevel(to); return { promise: Promise.resolve(), finish() {} }; }
    let stop = false, resolveFn;
    const promise = new Promise(r => { resolveFn = r; });
    const t0 = performance.now();
    const step = () => {
      const p = (performance.now() - t0) / (sec * 1000);
      if (stop || p >= 1) { this.setLevel(to); return resolveFn(); }
      this.setLevel(curve(from, to, p));
      setTimeout(step, 60);
    };
    step();
    return { promise, finish: () => { stop = true; } };
  }

  pause() { return this.player && within(this.player.pause()); }
  resume() { return this.player && within(this.player.resume()); }
  seek(ms) { return this.player && within(this.player.seek(Math.round(ms))); }
  async stop() { const had = this.track; this.track = null; if (had && this.player) await within(this.player.pause()); }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------
const engine = {
  cur: null,     // { deck, track }
  nxt: null,     // { deck, track, ready, error, promise }
  trans: null,   // { from, to, faders: [] }
  paused: false,
  loading: null, // message while a track is being fetched
  chain: Promise.resolve(),

  run(fn) {
    const job = this.chain.then(fn).catch(e => { if (e.message !== 'superseded') log(e.message, 'error'); });
    // Watchdog: even if a step hangs, later commands (skip, pause…) still get their turn.
    let finished = false;
    job.finally(() => { finished = true; });
    this.chain = Promise.race([job, sleep(30000).then(() => { if (!finished) log('A player step took too long; moving on', 'error'); })]);
    job.finally(() => { report(); render(); });
    return job;
  },

  // Long waits (a song still downloading, Spotify starting) give way as soon as a newer
  // transport command arrives, instead of making it queue up behind them.
  gen: 0,
  lastInterrupt: null,
  abortWaiters: new Set(),
  interrupt(type) {
    this.gen++;
    this.lastInterrupt = type;
    for (const r of this.abortWaiters) r();
    this.abortWaiters.clear();
  },
  async wait(promise) {
    let abort;
    const aborted = new Promise(r => { abort = () => r(ABORTED); });
    this.abortWaiters.add(abort);
    Promise.resolve(promise).catch(() => {}); // a late failure after we gave up isn't an error
    try { return await Promise.race([promise, aborted]); }
    finally { this.abortWaiters.delete(abort); }
  },
  skipPast: null,

  queue() { return S.state.queue || []; },
  find(uid) { return this.queue().find(t => t.uid === uid); },
  playable(t) { return t && !S.failed.has(t.uid) && !['drm', 'unavailable'].includes(t.playable) && (t.source !== 'spotify' || sp.ready); },
  after(uid, skipBad = true) {
    const q = this.queue();
    let i = uid ? q.findIndex(t => t.uid === uid) : -1;
    if (uid && i < 0) return null;
    for (let j = i + 1; j < q.length; j++) if (!skipBad || this.playable(q[j])) return q[j];
    return null;
  },
  before(uid) {
    const q = this.queue();
    const i = q.findIndex(t => t.uid === uid);
    for (let j = i - 1; j >= 0; j--) if (this.playable(q[j])) return q[j];
    return null;
  },

  fadeSeconds(a, b) {
    const s = S.state.settings || {};
    if (!a || !b) return 0;
    if (s.spotifySafe && (a.source === 'spotify' || b.source === 'spotify')) return 0;
    let f = Number(s.crossfade) || 0;
    const shortest = Math.min(a.duration || 1e9, b.duration || 1e9) / 1000;
    return Math.max(0, Math.min(f, shortest * 0.4));
  },

  freeScDeck() {
    const busy = new Set([this.cur && this.cur.deck]);
    return busy.has(scA) ? scB : scA;
  },

  releaseNext() {
    if (this.nxt && this.nxt.deck !== sp && this.nxt.deck !== (this.cur && this.cur.deck)) this.nxt.deck.unload();
    this.nxt = null;
  },

  // Decide what plays next and start buffering it.
  prepareNext() {
    if (this.trans) return;
    const t = this.cur ? this.after(this.cur.track.uid) : null;
    if (!t) { this.releaseNext(); return; }
    if (this.nxt && this.nxt.track.uid === t.uid) return;
    this.releaseNext();
    if (t.source === 'spotify') {
      this.nxt = { deck: sp, track: t, ready: true };
    } else {
      const deck = this.freeScDeck();
      const n = this.nxt = { deck, track: t, ready: false };
      n.promise = deck.load(t).then(() => {
        n.ready = true;
        log(`Preloaded: ${t.title}`);
      }).catch(e => {
        if (e.message === 'superseded') return;
        n.error = e.message;
        S.failed.add(t.uid);
        log(`Skipping "${t.title}": ${e.message}`, 'error');
        if (this.nxt === n) { this.nxt = null; this.prepareNext(); }
      }).finally(render);
    }
    this.warmCache(t);
    render();
  },

  // Ask the server to download a few more SoundCloud/YouTube tracks ahead of time.
  warmCache(from) {
    let t = from, n = (S.state.settings && S.state.settings.preloadAhead) || 2;
    while (n-- > 0 && (t = this.after(t.uid))) {
      if (t.source !== 'spotify') api(`/api/prepare/${t.source}/${encodeURIComponent(t.id)}`, { method: 'POST' }).catch(() => {});
    }
  },

  // Called ~10x/second.
  tick() {
    if (!this.cur || this.paused || this.trans || this.busy) return;
    const d = this.cur.deck;
    const dur = d.duration, pos = d.position;
    if (!dur || !d.playing) return;
    const remaining = dur - pos;
    const n = this.nxt;
    if (n && n.ready) {
      const f = this.fadeSeconds(this.cur.track, n.track);
      let lead = n.deck === sp ? 1200 : 0;              // Spotify needs a moment to start
      if (d === sp && n.deck === sp) lead = 0;
      const at = (d === sp && n.deck === sp) ? Math.max(f / 2, 0.25) * 1000 : f * 1000 + lead;
      if (remaining <= Math.max(at, d === sp ? 350 : 60)) {
        this.busy = true;
        this.run(() => this.transition(f)).finally(() => { this.busy = false; });
      }
    } else if (d === sp && remaining <= 350) {
      // Spotify has no reliable "ended" event; treat the last moment as the end.
      this.busy = true;
      this.run(() => this.onEndedInner(d)).finally(() => { this.busy = false; });
    }
  },

  onEnded(deck) { if (this.cur && this.cur.deck === deck && !this.trans) this.run(() => this.onEndedInner(deck)); },

  async onEndedInner(deck) {
    if (!this.cur || this.cur.deck !== deck) return;
    const n = this.nxt;
    if (n && !n.ready && n.promise) { this.loading = 'Loading ' + n.track.title; render(); await n.promise; this.loading = null; }
    if (this.nxt && this.nxt.ready) return this.transition(0);
    log('End of playlist');
    await deck.stop();
    this.cur = null;
    report({ currentUid: S.state.currentUid });
  },

  // Crossfade from the current deck to the prepared next deck.
  async transition(f) {
    const out = this.cur, inn = this.nxt;
    if (!out || !inn || !inn.ready) return;
    this.nxt = null;
    const tr = this.trans = { from: out.track.uid, to: inn.track.uid, sec: f, faders: [] };
    const fade = (deck, to, sec) => { const x = deck.fade(to, sec); tr.faders.push(x); return x.promise; };
    this.cur = inn;
    log(`${f ? 'Crossfading' : 'Next'}: ${inn.track.title}${f ? ` (${f.toFixed(1)}s)` : ''}`);
    try {
      if (out.deck === sp && inn.deck === sp) {
        // One Spotify stream at a time: fade out, then fade the new track in.
        if (f > 0) await fade(sp, 0, Math.min(f / 2, 4));
        report({ currentUid: inn.track.uid, transition: tr });
        await sp.start(inn.track, f > 0 ? 0 : 1);
        if (f > 0) await fade(sp, 1, Math.min(f / 2, 2));
      } else if (inn.deck === sp) {
        await sp.start(inn.track, f > 0 ? 0 : 1);
        report({ currentUid: inn.track.uid, transition: tr });
        const left = Math.max(0, (out.deck.duration - out.deck.position) / 1000);
        const sec = f > 0 ? Math.max(0.5, Math.min(f, left)) : 0;
        await Promise.all([fade(out.deck, 0, sec), fade(sp, 1, sec)]);
        out.deck.stop();
      } else {
        inn.deck.setLevel(f > 0 ? 0 : 1);
        await inn.deck.play();
        report({ currentUid: inn.track.uid, transition: tr });
        await Promise.all([fade(out.deck, 0, f), fade(inn.deck, 1, f)]);
        await out.deck.stop();
      }
    } catch (e) {
      S.failed.add(inn.track.uid);
      log(`Could not play "${inn.track.title}": ${e.message}`, 'error');
      if (out.deck !== inn.deck) {
        // Keep the old track going if it still has time left.
        this.cur = out.deck.playing ? out : null;
        if (this.cur) out.deck.setLevel(1);
        if (inn.deck !== sp) inn.deck.unload(); else sp.stop();
      } else {
        this.cur = null;
      }
    } finally {
      this.trans = null;
      this.prepareNext();
      report({ currentUid: this.cur ? this.cur.track.uid : S.state.currentUid });
    }
  },

  finishTransition() { if (this.trans) for (const f of this.trans.faders) f.finish(); },

  // Hard start of a specific track (user tapped it, or first play).
  async playTrack(t) {
    if (!t) return;
    this.paused = false;
    if (!this.playable(t) && t.source === 'spotify' && !sp.ready) throw new Error('Spotify is not connected; log in from Settings on this computer');
    S.failed.delete(t.uid);
    // Already prepared? Use a quick crossfade instead of a hard cut.
    if (this.cur && this.nxt && this.nxt.track.uid === t.uid && this.nxt.ready && this.cur.deck.playing) {
      return this.transition(Math.min(2, this.fadeSeconds(this.cur.track, t)));
    }
    const old = this.cur;
    this.cur = null;
    this.releaseNext();
    if (old) { if (old.deck.playing) await old.deck.fade(0, 0.35).promise; await old.deck.stop(); }
    const deck = t.source === 'spotify' ? sp : scA;
    this.cur = { deck, track: t };
    report({ currentUid: t.uid, playing: false, position: 0, duration: t.duration, message: 'Loading…' });
    this.loading = 'Loading ' + t.title; render();
    report({ message: this.loading });
    try {
      const loaded = await this.wait(deck === sp ? sp.start(t, 1) : deck.load(t));
      if (loaded === ABORTED) {
        // A newer command (another skip, stop…) arrived; let it take over.
        this.loading = null;
        if (deck !== sp) deck.unload(); else sp.stop();
        if (this.cur && this.cur.track.uid === t.uid) this.cur = { deck, track: t, loadingAborted: true };
        return;
      }
      if (deck !== sp) { deck.setLevel(1); await deck.play(); }
      log(`Playing: ${t.title}`);
    } catch (e) {
      this.cur = null;
      S.failed.add(t.uid);
      this.loading = null;
      if (e.message === 'superseded') return;
      log(`Could not play "${t.title}": ${e.message}`, 'error');
      const n = this.after(t.uid);
      if (n) return this.playTrack(n);
      return;
    }
    this.loading = null;
    this.prepareNext();
  },

  async command(c) {
    let cur = this.cur;
    if (cur && cur.loadingAborted && c.type !== 'next' && c.type !== 'prev') {
      // The current song never finished loading; start it again for play/resume.
      if (c.type === 'resume' || c.type === 'toggle') { this.cur = null; return this.playTrack(cur.track); }
    }
    switch (c.type) {
      case 'play': return this.playTrack(this.find(c.uid));
      case 'toggle': return this.paused || !cur ? this.command({ type: 'resume' }) : this.command({ type: 'pause' });
      case 'pause':
        if (cur) { await cur.deck.pause(); this.paused = true; log('Paused'); }
        return;
      case 'resume':
        if (!cur) return this.playTrack(this.find(S.state.currentUid) || this.queue().find(t => this.playable(t)));
        await cur.deck.resume(); this.paused = false; log('Resumed');
        return;
      case 'next': {
        if (!cur) return this.playTrack(this.after(S.state.currentUid) || this.queue()[0]);
        const n = this.nxt;
        // Skip pressed again while the next song was still downloading: jump past it.
        if (this.skipPast && n && n.track.uid === this.skipPast) {
          this.skipPast = null;
          const beyond = this.after(n.track.uid);
          log(`Skipped "${n.track.title}" while it was still loading`);
          if (beyond) return this.playTrack(beyond);
          return;
        }
        this.skipPast = null;
        if (n && !n.ready && n.promise) {
          this.loading = 'Loading next song: ' + n.track.title;
          render(); report();
          const r = await this.wait(n.promise);
          this.loading = null;
          if (r === ABORTED) {
            if (this.lastInterrupt === 'next') this.skipPast = n.track.uid;
            return;
          }
        }
        if (this.nxt && this.nxt.ready && !this.paused && cur.deck.playing) return this.transition(Math.min(2, this.fadeSeconds(cur.track, this.nxt.track)));
        const target = this.nxt && this.nxt.ready ? this.nxt.track : this.after(cur.track.uid);
        if (target) return this.playTrack(target);
        return;
      }
      case 'prev': {
        if (cur && cur.deck.position > 5000) { await cur.deck.seek(0); return; }
        const p = this.before(cur ? cur.track.uid : S.state.currentUid);
        if (p) return this.playTrack(p);
        if (cur) await cur.deck.seek(0);
        return;
      }
      case 'seek': if (cur) await cur.deck.seek(c.ms); return;
      case 'stop':
        if (cur) { await cur.deck.fade(0, 0.3).promise; await cur.deck.stop(); }
        this.cur = null; this.releaseNext(); this.paused = false;
        report({ currentUid: c.clear ? null : S.state.currentUid });
        return;
      case 'volume': setMaster(c.value); return;
      case 'replaced':
        log('Another player tab took over. This one is now idle.', 'error');
        es.close();
        if (cur) await cur.deck.stop();
        this.cur = null; this.releaseNext();
        document.body.classList.add('replaced');
        return;
    }
  },

  onQueueChanged() {
    if (this.nxt && !this.find(this.nxt.track.uid)) this.releaseNext();
    this.prepareNext();
  },
};

function setMaster(v) {
  S.master = Math.max(0, Math.min(1, v));
  masterGain.gain.setTargetAtTime(S.master, ctx.currentTime, 0.05);
  sp.applyMaster();
  $('#vol').value = S.master;
  report({ volume: S.master });
}

// ---------------------------------------------------------------------------
// Server link
// ---------------------------------------------------------------------------
let es, lastReport = 0;
function report(extra = {}) {
  const c = engine.cur;
  const body = {
    currentUid: c ? c.track.uid : (extra.currentUid !== undefined ? extra.currentUid : S.state.currentUid),
    playing: !!(c && c.deck.playing && !engine.paused),
    position: c ? Math.round(c.deck.position) : 0,
    duration: c ? Math.round(c.deck.duration) : 0,
    message: engine.loading || '',
    transition: engine.trans ? { from: engine.trans.from, to: engine.trans.to, sec: engine.trans.sec } : null,
    next: engine.nxt ? { uid: engine.nxt.track.uid, ready: !!engine.nxt.ready } : null,
    volume: S.master,
    host: HOST,
    ...extra,
  };
  if (extra.transition) body.transition = { from: extra.transition.from, to: extra.transition.to, sec: extra.transition.sec };
  if (body.currentUid !== S.state.currentUid) S.state.currentUid = body.currentUid;
  lastReport = Date.now();
  fetch('/api/player/report', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }).catch(() => {});
}

function connect() {
  es = new EventSource('/api/events?role=player');
  es.addEventListener('state', e => {
    const st = JSON.parse(e.data);
    const qChanged = JSON.stringify(st.queue.map(t => t.uid)) !== JSON.stringify(S.state.queue.map(t => t.uid));
    S.state = { ...st, currentUid: engine.cur ? engine.cur.track.uid : st.currentUid };
    if (qChanged) engine.onQueueChanged();
    render();
  });
  es.addEventListener('info', e => {
    const info = JSON.parse(e.data);
    // Logged in to Spotify after the player started: bring the Spotify deck up.
    if (info.spotify && info.spotify.loggedIn && sp && !sp.player && !sp.initializing) {
      if (!engine.cur) location.reload();
      else sp.init().catch(err => { sp.error = err.message; }).finally(render);
    }
  });
  es.addEventListener('command', e => {
    const c = JSON.parse(e.data);
    // A skip during a long crossfade shouldn't wait for it: jump the fades to their end now.
    if (c.type !== 'volume') engine.finishTransition();
    if (['next', 'prev', 'play', 'stop', 'pause', 'toggle', 'resume'].includes(c.type)) engine.interrupt(c.type);
    engine.run(() => engine.command(c));
  });
  es.onerror = () => { $('#conn').textContent = 'Reconnecting to server…'; };
  es.onopen = () => { $('#conn').textContent = ''; report(); };
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------
function trackCard(t, label, extra = '') {
  if (!t) return `<div class="card empty"><div class="lbl">${label}</div><div class="muted">Nothing queued</div></div>`;
  return `<div class="card">
    <div class="art" style="background-image:url('${esc(t.artwork)}')"></div>
    <div class="meta"><div class="lbl">${label} <span class="badge ${t.source}">${SOURCE_LABEL[t.source]}</span></div>
      <div class="title">${esc(t.title)}</div><div class="artist">${esc(t.artist)}</div>${extra}</div></div>`;
}

let renderKey = '';
function render() {
  if (!scA) return;
  const c = engine.cur, n = engine.nxt;
  const now = c ? c.track : engine.find(S.state.currentUid);
  const label = engine.trans ? 'Crossfading into' : engine.paused ? 'Paused' : 'Now playing';
  const upcoming = n ? n.track : engine.after(now && now.uid);
  const key = [now && now.uid, label, !!c, upcoming && upcoming.uid, n && n.ready, n && n.error].join('|');
  if (key !== renderKey) {
    // Rebuild the cards only when something changed; progress updates in place below.
    renderKey = key;
    const extra = c ? `<div class="bar"><div></div></div><div class="times"><span class="pos"></span><span class="dur"></span></div>` : '';
    $('#now').innerHTML = trackCard(now, label, extra);
    let ns = '';
    if (n) ns = `<div class="pill ${n.ready ? 'ok' : n.error ? 'bad' : 'wait'}">${n.ready ? (n.deck === sp ? 'Ready (Spotify starts on cue)' : 'Buffered, ready to crossfade') : n.error ? esc(n.error) : 'Preloading…'}</div>`;
    $('#next').innerHTML = trackCard(upcoming, 'Up next', ns);
  }
  if (c) {
    const pos = c.deck.position, dur = c.deck.duration;
    const bar = $('#now .bar > div');
    if (bar) { bar.style.width = (dur ? Math.min(100, (pos / dur) * 100) : 0) + '%'; $('#now .pos').textContent = fmtTime(pos); $('#now .dur').textContent = fmtTime(dur); }
  }
  const showPause = !!(c && !engine.paused);
  if ($('#playbtn').dataset.icon !== String(showPause)) {
    $('#playbtn').innerHTML = showPause ? ICON.pause : ICON.play;
    $('#playbtn').dataset.icon = String(showPause);
    $('#playbtn').title = showPause ? 'Pause' : 'Play';
  }
  $('#loading').textContent = engine.loading || '';
  const spEl = $('#spstatus');
  spEl.className = 'pill ' + (sp.ready ? 'ok' : sp.error ? 'bad' : 'wait');
  spEl.textContent = sp.ready ? 'Spotify connected' : sp.error || 'Spotify connecting…';
  $('#xf').textContent = (S.state.settings.crossfade || 0) + 's crossfade' + (S.state.settings.spotifySafe ? ' (Spotify-safe on)' : '');
}

async function showJoin() {
  const info = await api('/api/info');
  const url = info.lanUrls[0];
  const box = $('#join');
  if (!url) { box.innerHTML = '<div class="muted">No network address found. Phones need to be on the same Wi-Fi as this computer.</div>'; return; }
  const qr = qrcode(0, 'M'); qr.addData(url); qr.make();
  box.innerHTML = `<div class="qr">${qr.createSvgTag({ cellSize: 4, margin: 2, scalable: true })}</div>
    <div><div class="lbl">Control from your phone</div><div class="url">${info.lanUrls.map(esc).join('<br>')}</div>
    <div class="muted small">Same Wi-Fi as this computer. Scan the code or type the address.</div></div>`;
}

async function start() {
  $('#start').disabled = true;
  ctx = new (window.AudioContext || window.webkitAudioContext)({ latencyHint: 'playback' });
  await Promise.race([ctx.resume(), new Promise(r => setTimeout(r, 1500))]);
  if (ctx.state !== 'running') {
    // This browser still wants a click before playing sound.
    $('#start').disabled = false;
    $('#gate-note').textContent = 'Your browser needs one click before MixDeck can play sound.';
    await new Promise(res => $('#start').addEventListener('click', () => ctx.resume().then(res), { once: true }));
  }
  masterGain = ctx.createGain();
  masterGain.connect(ctx.destination);
  scA = new ScDeck('A'); scB = new ScDeck('B');
  // A built-in web view without AAC playback can't play SoundCloud/YouTube: use the browser.
  if (HOST === 'app') {
    const aac = 'audio/mp4; codecs="mp4a.40.2"';
    const ok = (window.MediaSource && MediaSource.isTypeSupported(aac)) || new Audio().canPlayType(aac);
    if (!ok) drmFailed('this window can\'t play AAC audio');
  }
  sp = new SpotifyDeck();
  document.body.classList.add('started');
  connect();
  showJoin().catch(() => {});
  sp.init().catch(e => { sp.error = e.message; log(e.message, 'error'); }).finally(() => { render(); engine.prepareNext(); });

  // Timers in a worker keep ticking when this tab is in the background.
  const worker = new Worker(URL.createObjectURL(new Blob(['setInterval(() => postMessage(0), 100)'], { type: 'text/javascript' })));
  worker.onmessage = () => {
    engine.tick();
    if (Date.now() - lastReport > 1000) report();
  };
  setInterval(render, 500);
  render();
}

document.addEventListener('DOMContentLoaded', () => {
  $('#prevbtn').innerHTML = ICON.prev;
  $('#nextbtn').innerHTML = ICON.next;
  $('#playbtn').innerHTML = ICON.play;
  const boot = () => start().catch(e => { log(e.message, 'error'); $('#start').disabled = false; });
  if (AUTO) boot();
  else $('#start').addEventListener('click', boot, { once: true });
  if (HOST === 'browser') document.title = 'MixDeck Player (keep open)';
  $('#playbtn').addEventListener('click', () => { engine.finishTransition(); engine.interrupt('toggle'); engine.run(() => engine.command({ type: 'toggle' })); });
  $('#nextbtn').addEventListener('click', () => { engine.finishTransition(); engine.interrupt('next'); engine.run(() => engine.command({ type: 'next' })); });
  $('#prevbtn').addEventListener('click', () => { engine.finishTransition(); engine.interrupt('prev'); engine.run(() => engine.command({ type: 'prev' })); });
  $('#vol').addEventListener('input', e => setMaster(Number(e.target.value)));
  window.addEventListener('beforeunload', e => { if (engine.cur && engine.cur.deck.playing) { e.preventDefault(); e.returnValue = ''; } });
});

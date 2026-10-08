// First-run setup wizard for the MixDeck desktop app.
'use strict';

const $ = s => document.querySelector(s);
const W = {
  info: null,
  step: 0,
  use: { spotify: true, soundcloud: true, youtube: true },
  timers: [],
};

let toastTimer;
function toast(msg, err = false) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = 'toast show' + (err ? ' err' : '');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.className = 'toast'; }, err ? 4500 : 2000);
}
const openExternal = url => api('/api/open-url', { method: 'POST', body: { url } }).catch(e => toast(e.message, true));
const copy = async text => { try { await navigator.clipboard.writeText(text); toast('Copied'); } catch { toast('Select the text and press Ctrl+C', true); } };
const saveSettings = body => api('/api/settings', { method: 'POST', body }).then(i => { W.info = i; return i; });
function clearTimers() { W.timers.forEach(clearInterval); W.timers = []; }

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------
const STEPS = [
  { id: 'welcome', label: 'Welcome' },
  { id: 'spotify', label: 'Spotify', when: () => W.use.spotify, skippable: true },
  { id: 'soundcloud', label: 'SoundCloud', when: () => W.use.soundcloud, skippable: true },
  { id: 'youtube', label: 'YouTube', when: () => W.use.youtube },
  { id: 'phone', label: 'Your phone' },
  { id: 'prefs', label: 'Playback' },
  { id: 'done', label: 'Done' },
];
const active = () => STEPS.filter(s => !s.when || s.when());

function renderDots() {
  const list = active();
  const cur = list.indexOf(STEPS[W.step]);
  $('#dots').innerHTML = list.map((s, i) => `<div class="dot ${i === cur ? 'on' : i < cur ? 'done' : ''}"><b>${i < cur ? '✓' : i + 1}</b><span>${s.label}</span></div>`).join('');
}

function go(delta) {
  clearTimers();
  let i = W.step;
  do { i += delta; } while (i > 0 && i < STEPS.length - 1 && STEPS[i].when && !STEPS[i].when());
  W.step = Math.max(0, Math.min(STEPS.length - 1, i));
  render();
}

function render() {
  const step = STEPS[W.step];
  renderDots();
  $('#back').style.visibility = W.step === 0 ? 'hidden' : 'visible';
  $('#skip').style.display = step.skippable ? '' : 'none';
  $('#next').textContent = step.id === 'done' ? 'Open MixDeck' : step.id === 'welcome' ? "Let's go" : 'Next';
  $('#next').disabled = false;
  window.scrollTo(0, 0);
  VIEWS[step.id]($('#wz'));
}

const VIEWS = {
  welcome(box) {
    box.innerHTML = `
      <h1>Welcome to MixDeck</h1>
      <p class="lead">Build one playlist from Spotify, SoundCloud and YouTube, play it on this PC with smooth crossfades, and control it from your phone. This takes about five minutes.</p>
      <h3>Which services do you want to use?</h3>
      <label class="choice"><input type="checkbox" data-svc="spotify" ${W.use.spotify ? 'checked' : ''}><div><div class="t">Spotify</div><div class="d">Needs Spotify Premium and a free Spotify developer app. The next step walks you through it.</div></div></label>
      <label class="choice"><input type="checkbox" data-svc="soundcloud" ${W.use.soundcloud ? 'checked' : ''}><div><div class="t">SoundCloud</div><div class="d">Works right away. Add your profile to see your likes and playlists.</div></div></label>
      <label class="choice"><input type="checkbox" data-svc="youtube" ${W.use.youtube ? 'checked' : ''}><div><div class="t">YouTube</div><div class="d">MixDeck downloads two small helper tools for this.</div></div></label>
      <h3>Used the earlier version of MixDeck?</h3>
      <p class="muted">Bring over your Spotify login, SoundCloud profile, settings and saved playlists from the old MixDeck folder.</p>
      <div class="row-actions"><button class="ghost" id="import">Import from the old MixDeck folder</button></div>
      <div id="import-result"></div>`;
    box.querySelectorAll('[data-svc]').forEach(cb => cb.onchange = () => { W.use[cb.dataset.svc] = cb.checked; renderDots(); });
    $('#import').onclick = async () => {
      try {
        const r = await api('/api/import', { method: 'POST' });
        if (r.cancelled) return;
        W.info = await api('/api/info');
        $('#import-result').innerHTML = `<div class="card-row"><div class="big-check" style="font-size:28px;margin:0">✓</div><div>Imported ${esc([...r.imported, r.playlists ? r.playlists + ' saved playlists' : ''].filter(Boolean).join(', ') || 'nothing new')}.<div class="muted small">${esc(r.from)}</div></div></div>`;
      } catch (e) { $('#import-result').innerHTML = `<p class="err">${esc(e.message)}</p>`; }
    };
  },

  spotify(box) {
    const sp = W.info.spotify;
    if (sp.loggedIn) {
      box.innerHTML = `<h1>Spotify</h1><div class="card-row"><div class="big-check" style="font-size:32px;margin:0">✓</div><div><b>Connected${sp.user ? ' as ' + esc(sp.user.name) : ''}</b><div class="muted small">You're all set. You can change this later in Settings.</div></div></div>
        <div class="row-actions"><button class="ghost" id="sp-redo">Use a different Spotify app or account</button></div>`;
      $('#sp-redo').onclick = async () => { await api('/api/spotify/logout', { method: 'POST' }); W.info = await api('/api/info'); render(); };
      return;
    }
    box.innerHTML = `
      <h1>Connect Spotify</h1>
      <p class="lead">Spotify only lets personal projects like MixDeck play music through a free "developer app" that you create. You need <b>Spotify Premium</b>.</p>
      <ol class="steps">
        <li id="s1"><b>Open the Spotify developer dashboard</b><p>Log in with your normal Spotify account and accept the terms if asked.</p>
          <button class="ghost" id="open-dash">Open developer.spotify.com/dashboard ↗</button></li>
        <li id="s2"><b>Create an app</b><p>Click <b>Create app</b>. Use any name and description, for example "MixDeck". Paste this as the <b>Redirect URI</b> and click Add:</p>
          <div class="copy"><code>${esc(sp.redirectUri)}</code><button class="ghost" id="copy-uri">Copy</button></div>
          <p>Under "Which API/SDKs are you planning to use", tick <b>Web API</b> and <b>Web Playback SDK</b>, then save.</p></li>
        <li id="s3"><b>Allow your account</b><p>In the app's <b>User Management</b> tab, add the name and email of your Spotify account.</p></li>
        <li id="s4" class="${sp.clientId ? 'ok' : ''}"><b>Paste the Client ID</b><p>On the app's <b>Settings</b> page, copy the <b>Client ID</b> and paste it here.</p>
          <div class="inline" style="display:flex;gap:8px"><input type="text" id="cid" value="${esc(sp.clientId)}" placeholder="32-character Client ID" spellcheck="false"><button class="ghost" id="cid-save">Save</button></div>
          <div id="cid-msg"></div></li>
        <li id="s5"><b>Log in</b><p>Your browser opens Spotify's login. Approve MixDeck, then come back here; this page notices automatically.</p>
          <button class="primary" id="login" ${sp.clientId ? '' : 'disabled'}>Log in with Spotify</button>
          <div id="login-msg"></div></li>
      </ol>`;
    $('#open-dash').onclick = () => openExternal('https://developer.spotify.com/dashboard');
    $('#copy-uri').onclick = () => copy(sp.redirectUri);
    $('#cid-save').onclick = async () => {
      const v = $('#cid').value.trim();
      if (!/^[0-9a-f]{32}$/i.test(v)) { $('#cid-msg').innerHTML = '<p class="err">That doesn\'t look like a Client ID. It\'s 32 letters and numbers, shown on the app\'s Settings page.</p>'; return; }
      await saveSettings({ spotifyClientId: v });
      $('#cid-msg').innerHTML = '';
      $('#s4').classList.add('ok');
      $('#login').disabled = false;
      toast('Saved');
    };
    $('#login').onclick = () => {
      openExternal('/auth/spotify/login');
      $('#login-msg').innerHTML = '<p class="muted">Waiting for you to approve MixDeck in your browser…</p>';
      clearTimers();
      W.timers.push(setInterval(async () => {
        const i = await api('/api/info').catch(() => null);
        if (i && i.spotify.loggedIn) { W.info = i; clearTimers(); toast('Spotify connected'); render(); }
      }, 2000));
    };
  },

  soundcloud(box) {
    const p = W.info.soundcloud.profile;
    box.innerHTML = `
      <h1>Your SoundCloud</h1>
      <p class="lead">SoundCloud search works without an account. To see your likes and playlists, paste your profile link.</p>
      <ol class="steps">
        <li><b>Copy your profile link</b><p>On soundcloud.com, open your profile and copy the address bar, for example <code>https://soundcloud.com/yourname</code>. In the SoundCloud app: your profile → Share → Copy link.</p>
          <button class="ghost" id="open-sc">Open soundcloud.com ↗</button></li>
        <li class="${p ? 'ok' : ''}" id="sc2"><b>Paste it here</b>
          <div style="display:flex;gap:8px;margin-top:8px"><input type="text" id="sc" value="${esc(p ? p.url : '')}" placeholder="https://soundcloud.com/yourname" spellcheck="false"><button class="ghost" id="sc-save">Find</button></div>
          <div id="sc-msg"></div>
          ${p ? profileCard(p) : ''}</li>
      </ol>`;
    $('#open-sc').onclick = () => openExternal('https://soundcloud.com/you');
    const find = async () => {
      const v = $('#sc').value.trim();
      if (!v) return;
      $('#sc-save').disabled = true;
      $('#sc-msg').innerHTML = '<p class="muted">Looking up your profile…</p>';
      try { await saveSettings({ scProfile: v }); render(); }
      catch (e) { $('#sc-msg').innerHTML = `<p class="err">${esc(e.message)}</p>`; $('#sc-save').disabled = false; }
    };
    $('#sc-save').onclick = find;
    $('#sc').addEventListener('keydown', e => { if (e.key === 'Enter') find(); });
  },

  youtube(box) {
    box.innerHTML = `
      <h1>YouTube</h1>
      <p class="lead">MixDeck uses <b>yt-dlp</b>, a free open-source tool, to play YouTube audio. It also needs a JavaScript runtime: your installed Node.js if you have version 22 or newer, otherwise MixDeck downloads <b>Deno</b>. Both are saved in MixDeck's data folder.</p>
      <div class="card-row" style="display:block">
        <div id="yt-step"><b>Checking…</b></div>
        <div class="progress"><div id="yt-bar"></div></div>
        <div id="yt-detail" class="muted small"></div>
      </div>
      <div class="row-actions"><button class="ghost" id="yt-retry" style="display:none">Try again</button></div>
      <p class="muted small">You can continue while this downloads. If YouTube songs stop loading in the future, use Settings → YouTube → Update yt-dlp.</p>`;
    let started = false;
    const poll = async () => {
      const t = await api('/api/tools/status').catch(() => null);
      if (!t) return;
      const ins = t.install || {};
      const bar = $('#yt-bar'); if (!bar) return;
      if (t.ready && !ins.running) {
        $('#yt-step').innerHTML = '<b style="color:var(--ok)">✓ YouTube is ready</b>';
        bar.style.width = '100%';
        $('#yt-detail').textContent = `yt-dlp ${t.ytdlp.version || ''} · JavaScript runtime: ${t.js ? (t.js.kind === 'node' ? 'Node.js (already installed)' : 'Deno') : 'none'}`;
        $('#yt-retry').style.display = 'none';
        clearTimers();
      } else if (ins.error) {
        $('#yt-step').innerHTML = `<b style="color:var(--bad)">Download failed</b>`;
        $('#yt-detail').textContent = ins.error;
        $('#yt-retry').style.display = '';
        clearTimers();
      } else if (ins.running) {
        $('#yt-step').innerHTML = `<b>${esc(ins.step || 'Downloading')}…</b>`;
        bar.style.width = Math.round((ins.progress || 0) * 100) + '%';
        $('#yt-detail').textContent = Math.round((ins.progress || 0) * 100) + '%';
      } else if (!started) {
        started = true;
        await api('/api/tools/install', { method: 'POST' }).catch(e => { $('#yt-detail').textContent = e.message; });
      }
    };
    $('#yt-retry').onclick = () => { started = false; clearTimers(); W.timers.push(setInterval(poll, 700)); poll(); };
    W.timers.push(setInterval(poll, 700));
    poll();
  },

  phone(box) {
    const urls = W.info.lanUrls;
    box.innerHTML = `
      <h1>Control it from your phone</h1>
      <p class="lead">Any phone or computer on the same Wi-Fi can open MixDeck in its browser to search, build the queue and skip songs.</p>
      ${urls.length ? `<div class="card-row"><div class="qr" id="qr" style="width:150px;height:150px"></div><div><div class="lbl">Scan with your phone's camera, or type</div><div class="url" style="font-size:18px">${urls.map(esc).join('<br>')}</div><div class="muted small">On iPhone, use Share → Add to Home Screen to open it like an app.</div></div></div>`
        : '<div class="note">This PC doesn\'t seem to be on a network right now. Connect to Wi-Fi to use your phone as a remote.</div>'}
      <div class="note"><b>If Windows asks about the firewall</b>, tick <b>Private networks</b> and click <b>Allow</b>. Without that, phones can't reach MixDeck. If you missed it: Windows Security → Firewall &amp; network protection → Allow an app through firewall → tick MixDeck.</div>`;
    if (urls.length && window.qrcode) {
      const qr = qrcode(0, 'M'); qr.addData(urls[0]); qr.make();
      $('#qr').innerHTML = qr.createSvgTag({ cellSize: 4, margin: 2, scalable: true });
    }
  },

  prefs(box) {
    const s = W.info.settings;
    box.innerHTML = `
      <h1>Playback</h1>
      <p class="lead">You can change all of this later in Settings.</p>
      <div class="set">
        <label>Crossfade between songs: <b id="xf-val">${s.crossfade}s</b></label>
        <input type="range" id="xf" min="0" max="15" step="0.5" value="${s.crossfade}" style="width:100%">
        ${W.use.spotify ? `<label class="switch" style="margin-top:14px"><input type="checkbox" id="spsafe" ${s.spotifySafe ? 'checked' : ''}> Spotify-safe transitions</label>
        <p>Spotify's developer rules don't allow Spotify songs to overlap other audio. Turn this on to follow them: songs involving Spotify won't overlap, and the rest still crossfade.</p>` : ''}
      </div>
      <div class="set">
        <label class="switch"><input type="checkbox" id="autostart" ${s.startWithWindows ? 'checked' : ''}> Start MixDeck when Windows starts</label>
        <label class="switch"><input type="checkbox" id="closetray" ${s.closeToTray ? 'checked' : ''}> Keep playing in the system tray when I close the window</label>
        <p>MixDeck's icon sits in the tray by the clock. Right-click it for play/pause, next, or to quit.</p>
      </div>`;
    const xf = $('#xf');
    xf.oninput = () => { $('#xf-val').textContent = xf.value + 's'; };
    xf.onchange = () => saveSettings({ crossfade: Number(xf.value) });
    const ss = $('#spsafe'); if (ss) ss.onchange = () => saveSettings({ spotifySafe: ss.checked });
    $('#autostart').onchange = e => saveSettings({ startWithWindows: e.target.checked }).catch(err => { toast(err.message, true); e.target.checked = !e.target.checked; });
    $('#closetray').onchange = e => saveSettings({ closeToTray: e.target.checked });
  },

  done(box) {
    const i = W.info;
    const items = [
      W.use.spotify ? (i.spotify.loggedIn ? '✓ Spotify connected' : '– Spotify not connected yet (Settings → Spotify)') : null,
      W.use.soundcloud ? (i.soundcloud.profile ? `✓ SoundCloud: ${esc(i.soundcloud.profile.username)}` : '✓ SoundCloud search (no profile added)') : null,
      W.use.youtube ? '✓ YouTube' : null,
    ].filter(Boolean);
    box.innerHTML = `
      <div class="big-check">✓</div>
      <h1>You're ready</h1>
      <p class="lead">${items.join('<br>')}</p>
      <div class="set">
        <h3>How to use it</h3>
        <p><b>Search</b> any service, or paste a Spotify, SoundCloud or YouTube link. Tap <b>＋</b> to add songs to the queue, or drag the dotted handle to reorder.</p>
        <p><b>Library</b> has your Spotify and SoundCloud likes and playlists. <b>Playlists</b> saves mixes that combine all three.</p>
        <p>Music plays from this PC. Closing the window keeps the music going; MixDeck stays in the tray.</p>
      </div>`;
  },
};

function profileCard(p) {
  return `<div class="card-row"><div class="art" style="background-image:url('${esc(p.avatar || '')}')"></div><div><b>${esc(p.username)}</b><div class="muted small">soundcloud.com/${esc(p.permalink)}${p.likes != null ? ` · ${p.likes} likes` : ''}${p.playlists != null ? ` · ${p.playlists} playlists` : ''}</div></div></div>`;
}

// ---------------------------------------------------------------------------
$('#next').onclick = async () => {
  if (STEPS[W.step].id === 'done') {
    await api('/api/setup/done', { method: 'POST' }).catch(() => {});
    location.href = '/';
    return;
  }
  go(1);
};
$('#back').onclick = () => go(-1);
$('#skip').onclick = e => { e.preventDefault(); go(1); };

api('/api/info').then(i => {
  W.info = i;
  // Coming back to setup later: start from what's already configured.
  if (i.settings.setupDone) W.use.spotify = !!i.spotify.clientId || W.use.spotify;
  render();
}).catch(e => { $('#wz').innerHTML = `<p class="err">MixDeck's server isn't responding: ${esc(e.message)}</p>`; });

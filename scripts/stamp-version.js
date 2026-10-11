// CI: give the app this build's version (2.1.<run number>) so installers and Android
// see each build as newer than the last.
const fs = require('fs');
const f = 'src-tauri/tauri.conf.json';
const v = process.env.MIXDECK_VERSION;
if (!v) { console.log('MIXDECK_VERSION not set; leaving version alone'); process.exit(0); }
const j = JSON.parse(fs.readFileSync(f, 'utf8'));
j.version = v;
fs.writeFileSync(f, JSON.stringify(j, null, 2) + '\n');
console.log('version', v);

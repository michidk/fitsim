// Boot, hash router and connection status.
import { store, on, connect, setState } from './store.js';
import { get } from './api.js';
import { h } from './ui.js';

const PAGES = {
  dashboard: ['Dashboard', './pages/dashboard.js'],
  events: ['Event Log', './pages/events.js'],
  settings: ['Settings', './pages/settings.js'],
  setup: ['Wi-Fi setup', './pages/setup.js'],
};
const app = document.getElementById('app');
const $ = (id) => document.getElementById(id);
let offs = [], token = 0, curProv = null;

const sub = (topic, fn) => { offs.push(on(topic, fn)); };

// The device has no accept queue: a connection that arrives while every worker is busy is refused.
// A failed import is cached per URL, so each retry uses a fresh query string.
async function importPage(path) {
  for (let i = 0; ; i++) {
    try { return await import(i ? `${path}?r=${i}` : path); } catch (e) {
      if (i === 3) throw e;
      await new Promise((r) => setTimeout(r, 300 * (i + 1)));
    }
  }
}

async function route() {
  const my = ++token;
  offs.splice(0).forEach((f) => f());
  const prov = store.state ? store.state.system?.wifiMode === 'accessPoint' : null;
  curProv = prov;
  document.body.classList.toggle('prov', !!prov);
  let name = (location.hash.match(/^#\/(\w+)/) || [])[1];
  if (prov) name = 'setup';
  else if (!PAGES[name] || name === 'setup') { name = 'dashboard'; history.replaceState(null, '', '#/dashboard'); }
  for (const a of document.querySelectorAll('#nav a')) {
    if (a.getAttribute('href') === '#/' + name) { a.setAttribute('aria-current', 'page'); a.scrollIntoView?.({ block: 'nearest', inline: 'center' }); } else a.removeAttribute('aria-current');
  }
  document.title = `${PAGES[name][0]} - ESP32 Fitness Simulator`;
  if (!store.state) return;
  try {
    const mod = await importPage(PAGES[name][1]);
    if (my !== token) return;
    app.replaceChildren();
    scrollTo(0, 0);
    mod.mount(app, sub);
  } catch (e) {
    console.error(e);
    app.replaceChildren(h('div', { class: 'errbox' }, `Failed to load page: ${e.message}`));
  }
}

// ---- connection status ----------------------------------------------------
function conn() {
  const c = store.conn;
  const el = $('conn');
  el.className = 'conn ' + c;
  el.textContent = c === 'up' ? 'Connected' : c === 'down' ? 'Disconnected - reconnecting' : 'Connecting...';
  $('banner').hidden = c !== 'down';
}

on('conn', conn);
on('state', () => {
  if ((store.state.system?.wifiMode === 'accessPoint') !== curProv) route();
});
addEventListener('hashchange', route);

connect();
get('/api/state').then((s) => { if (!store.state) { setState(s); route(); } }).catch(() => {});
conn();
route();

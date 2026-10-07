// Central state: WebSocket with reconnect, shared snapshot, event log buffer, tiny pub/sub.
import { get, post } from './api.js';

export const store = {
  state: null, // latest /api/state snapshot, kept current by WS frames
  uptime: 0, // device uptime (ms) from the newest frame
  conn: 'connecting', // connecting | up | down
  events: [], // ascending by seq, capped
  lastSeq: 0,
};
const MAX_EVENTS = 1000;
const subs = {};

/** Topics: state (any non-telemetry change), telemetry, conn, log, log-reset. Returns unsubscribe. */
export function on(topic, fn) {
  (subs[topic] ||= new Set()).add(fn);
  return () => subs[topic].delete(fn);
}
const emit = (topic, d) => subs[topic]?.forEach((f) => { try { f(d); } catch (e) { console.error(topic, e); } });

// ---- time helpers ---------------------------------------------------------
const p2 = (n) => String(n).padStart(2, '0');
/** Event time (uptime ms) as local HH:MM:SS, or +h:mm:ss uptime when the wall clock is unknown. */
export function clock(t) {
  const off = store.state?.time?.wallOffsetMs;
  if (off == null) {
    const s = Math.max(0, (t / 1000) | 0);
    return `+${(s / 3600) | 0}:${p2(((s % 3600) / 60) | 0)}:${p2(s % 60)}`;
  }
  const d = new Date(t + off);
  return `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}`;
}

let lastSync = 0;
async function syncTime() {
  if (store.state?.time?.wallOffsetMs != null || Date.now() - lastSync < 4000) return;
  lastSync = Date.now();
  try {
    await post('/api/time', { epochMs: Date.now() });
    const s = await get('/api/state');
    store.state.time = s.time;
    emit('state', store.state);
  } catch { /* retried on next state frame */ }
}

// ---- event buffer -----------------------------------------------------------
const seen = new Set();
function addEvents(list) {
  const fresh = [];
  for (const e of list) {
    if (seen.has(e.seq)) continue;
    seen.add(e.seq);
    fresh.push(e);
  }
  if (!fresh.length) return;
  const ordered = fresh.every((e, i) => e.seq > (i ? fresh[i - 1].seq : store.lastSeq));
  store.events.push(...fresh);
  if (!ordered) store.events.sort((a, b) => a.seq - b.seq);
  while (store.events.length > MAX_EVENTS) seen.delete(store.events.shift().seq);
  store.lastSeq = Math.max(store.lastSeq, ...fresh.map((e) => e.seq));
  emit('log', { fresh, ordered });
}
function resetEvents() {
  store.events = [];
  store.lastSeq = 0;
  seen.clear();
  emit('log-reset');
}
let epoch = null;
export async function loadEvents(query = 'limit=200') {
  const r = await get('/api/events?' + query);
  if (epoch != null && r.epoch !== epoch) { // the device restarted: start over with its fresh log
    epoch = r.epoch;
    resetEvents();
    if (query !== 'limit=200') return loadEvents('limit=200');
  }
  epoch = r.epoch;
  addEvents(r.events);
}
async function syncEvents(deviceLast) {
  try {
    if (deviceLast < store.lastSeq) resetEvents(); // device restarted or log cleared
    await loadEvents(store.lastSeq ? `since=${store.lastSeq}&limit=200` : 'limit=200');
  } catch { /* log is non-critical */ }
}

// ---- websocket --------------------------------------------------------------
function setConn(c) {
  if (store.conn === c) return;
  store.conn = c;
  emit('conn', c);
}
export function setState(s) {
  store.state = s;
  store.uptime = s.time.uptimeMs;
}

function handle(m) {
  const d = m.data, st = store.state;
  if (m.timestamp != null) store.uptime = m.timestamp;
  switch (m.type) {
    case 'hello': setConn('up'); syncEvents(d.lastEventSeq); break;
    case 'state': setState(d); emit('state', d); syncTime(); break;
    case 'telemetry': if (st) { st.telemetry = d; emit('telemetry', d); } break;
    case 'log': addEvents([d]); break;
    case 'log-cleared': resetEvents(); break;
    case 'ble-state': if (st) { st.ble = d; emit('state', st); } break;
  }
}

let ws, backoff = 300, lastFrame = 0;
export function connect() {
  ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`);
  ws.onopen = () => { backoff = 300; lastFrame = Date.now(); };
  ws.onmessage = (e) => {
    lastFrame = Date.now();
    try { handle(JSON.parse(e.data)); } catch (err) { console.error('ws frame', err); }
  };
  ws.onclose = () => {
    setConn('down');
    setTimeout(connect, backoff);
    backoff = Math.min(backoff * 1.7, 5000);
  };
}
// Telemetry arrives every 200 ms; a silent socket is a dead socket.
setInterval(() => { if (store.conn === 'up' && Date.now() - lastFrame > 6000) ws.close(); }, 2000);

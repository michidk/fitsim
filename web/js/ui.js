// Tiny DOM helpers, formatters, toasts and dialogs. Everything is built with
// createElement/textContent so user-supplied strings can never inject markup.

export const METRICS = [
  { key: 'speed', slug: 'speed', label: 'Speed', d: 1, step: 0.1 },
  { key: 'cadence', slug: 'cadence', label: 'Cadence', d: 0, step: 1 },
  { key: 'power', slug: 'power', label: 'Power', d: 0, step: 1 },
  { key: 'heartRate', slug: 'heart-rate', label: 'Heart rate', d: 0, step: 1 },
  { key: 'resistance', slug: 'resistance', label: 'Resistance', d: 0, step: 1 },
];
export const DEVICES = [
  { key: 'trainer', label: 'Trainer', short: 'Trainer' },
  { key: 'heartRate', label: 'Heart Rate', short: 'HR' },
  { key: 'powerMeter', label: 'Power Meter', short: 'PM' },
];

function add(e, kids) {
  for (const k of kids.flat(9)) if (k != null && k !== false) e.append(k instanceof Node ? k : String(k));
  return e;
}

/** h('div', {class:'x', onclick: fn}, ...children) */
export function h(tag, props, ...kids) {
  const e = document.createElement(tag);
  for (const k in props) {
    const v = props[k];
    if (v == null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (k.includes('-')) e.setAttribute(k, v);
    else if (k in e) e[k] = v;
    else e.setAttribute(k, v === true ? '' : v);
  }
  return add(e, kids);
}

export const btn = (text, onclick, cls = '', extra) => h('button', { type: 'button', class: 'btn ' + cls, onclick, ...extra }, text);
export const field = (label, input, hint) => h('label', { class: 'f' }, h('span', null, label), input, hint && h('small', null, hint));
export const spinner = () => h('span', { class: 'spin', 'aria-hidden': 'true' });
export const card = (title, ...kids) => h('section', { class: 'card' }, title && h('h2', null, title), ...kids);

/** Toggle switch; returns the <label> with `.input` attached. */
export function sw(label, checked, onchange) {
  const input = h('input', { type: 'checkbox', checked, onchange: () => onchange(input.checked, input) });
  const el = h('label', { class: 'sw' }, input, h('span', { class: 'sw-t' }), h('span', null, label));
  el.input = input;
  return el;
}

// ---- formatting -----------------------------------------------------------
const p2 = (n) => String(n).padStart(2, '0');
export const fmt = (v, d = 0) => (v == null || !isFinite(v) ? '-' : Number(v).toFixed(d));
export function hms(sec) {
  sec = Math.max(0, Math.round(sec || 0));
  const hh = (sec / 3600) | 0, m = ((sec % 3600) / 60) | 0, s = sec % 60;
  return hh ? `${hh}:${p2(m)}:${p2(s)}` : `${p2(m)}:${p2(s)}`;
}
export const ago = (ms) => (ms < 1500 ? 'just now' : ms < 60000 ? `${Math.round(ms / 1000)} s ago` : `${hms(ms / 1000)} ago`);
export const devStatus = (d) => (d?.connected ? ['Connected', 'ok'] : ['Disconnected', 'idle']);
export const ASCII = /^[\x20-\x7e]{1,24}$/;

// ---- input behaviour ------------------------------------------------------
export function throttled(fn, ms = 100) {
  let last = 0, timer = null, args;
  const run = () => { timer = null; last = Date.now(); fn(...args); };
  const t = (...a) => {
    args = a;
    const wait = ms - (Date.now() - last);
    if (wait <= 0) { clearTimeout(timer); run(); } else if (!timer) timer = setTimeout(run, wait);
  };
  t.flush = (...a) => { args = a; clearTimeout(timer); run(); };
  return t;
}

/** Mark an input as user-controlled while dragged/typed so live updates do not fight it. */
export function lock(el) {
  const hold = () => { el._u = Date.now() + 900; };
  const up = () => { el._down = false; hold(); };
  el.addEventListener('pointerdown', () => { el._down = true; hold(); addEventListener('pointerup', up, { once: true }); });
  el.addEventListener('input', () => { el._typing = true; hold(); });
  el.addEventListener('change', () => { el._typing = false; hold(); });
  return el;
}
/** Set an input value from live data unless the user is using/editing it. */
export function setVal(el, v) {
  if (el._down || el._typing || el._u > Date.now() || el.dataset.dirty) return;
  v = String(v);
  if (el.value !== v) el.value = v;
}
/** Forms: inputs edited by the user stay untouched by live updates until `clean()`. */
export const dirtyTrack = (root) => root.addEventListener('input', (e) => { if (e.target.dataset) e.target.dataset.dirty = '1'; });
export const clean = (root) => root.querySelectorAll('[data-dirty]').forEach((e) => delete e.dataset.dirty);

// ---- toasts & dialogs -----------------------------------------------------
export function toast(msg, kind = 'ok') {
  const t = h('div', { class: 'toast ' + kind }, msg);
  document.getElementById('toasts').append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 6500 : 2800);
}

export function confirmBox(msg, { ok = 'Confirm', danger = false } = {}) {
  return new Promise((res) => {
    const d = h('dialog', { class: 'dlg' },
      h('form', { method: 'dialog' }, h('p', null, msg),
        h('div', { class: 'row end' },
          h('button', { class: 'btn', value: 'no', autofocus: true }, 'Cancel'),
          h('button', { class: 'btn ' + (danger ? 'danger' : 'pri'), value: 'ok' }, ok))));
    d.addEventListener('close', () => { d.remove(); res(d.returnValue === 'ok'); });
    document.body.append(d);
    d.showModal();
  });
}

export function overlay(title, text) {
  const body = h('p', null, text);
  const el = h('div', { class: 'overlay', role: 'alertdialog', 'aria-label': title },
    h('div', { class: 'card' }, h('h2', null, spinner(), ' ', title), body));
  document.body.append(el);
  return { set: (t) => { body.textContent = t; }, close: () => el.remove() };
}

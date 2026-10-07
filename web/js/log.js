// Event list renderer used by the Event Log page.
import { store, clock } from './store.js';
import { h } from './ui.js';

function eventRow(ev) {
  const bad = ev.ftms && ev.ftms.result && ev.ftms.result !== 'success';
  return h('div', { class: `ev ${ev.level}${bad ? ' bad' : ''}` },
    h('span', { class: 'tm' }, clock(ev.t)),
    h('span', { class: 'chip ' + ev.kind }, ev.kind),
    h('span', { class: 'msg' }, ev.message, bad && `  [${ev.ftms.result}]`, (ev.detail || []).map((d) => h('span', { class: 'det' }, d))));
}

/**
 * Scrolling log bound to store.events. `view.filter` selects what is shown;
 * `view.auto` follows new entries.
 */
export function logView(sub, { filter = () => true, empty = 'No events yet.' } = {}) {
  const el = h('div', { class: 'log', role: 'log', tabindex: 0, 'aria-label': 'Event log' });
  const view = { el, filter, auto: true, shown: 0, refresh, onchange: null };
  let known = store.state?.time?.wallOffsetMs != null;

  const stick = () => { if (view.auto) el.scrollTop = el.scrollHeight; };
  function refresh() {
    const list = store.events.filter(view.filter);
    el.replaceChildren(...(list.length ? list.map((e) => eventRow(e)) : [h('div', { class: 'empty' }, empty)]));
    view.shown = list.length;
    stick();
    view.onchange?.();
  }
  function append(fresh) {
    const list = fresh.filter(view.filter);
    if (!list.length) return;
    if (!view.shown) el.replaceChildren();
    el.append(...list.map((e) => eventRow(e)));
    view.shown += list.length;
    while (el.childElementCount > 1000) { el.firstChild.remove(); view.shown--; }
    stick();
    view.onchange?.();
  }
  sub('log', ({ fresh, ordered }) => (ordered ? append(fresh) : refresh()));
  sub('log-reset', refresh);
  sub('state', () => { // wall clock became known: redraw timestamps
    const k = store.state?.time?.wallOffsetMs != null;
    if (k !== known) { known = k; refresh(); }
  });
  refresh();
  return view;
}

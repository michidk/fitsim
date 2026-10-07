// Event log: live list with filters, search, autoscroll toggle, clear and download.
import { loadEvents } from '../store.js';
import { del, run } from '../api.js';
import { h, btn, card, field, confirmBox } from '../ui.js';
import { logView } from '../log.js';

const KINDS = ['ble', 'ftms', 'system', 'api'];
const LEVELS = { info: 0, warn: 1, error: 2 };

export function mount(root, sub) {
  const kind = h('select', null, h('option', { value: '' }, 'All kinds'), KINDS.map((k) => h('option', { value: k }, k)));
  const preset = new URLSearchParams(location.hash.split('?')[1]).get('kind');
  if (KINDS.includes(preset)) kind.value = preset;
  const level = h('select', null, h('option', { value: 'info' }, 'All levels'), h('option', { value: 'warn' }, 'Warnings and errors'), h('option', { value: 'error' }, 'Errors only'));
  const q = h('input', { type: 'search', placeholder: 'message or detail', autocomplete: 'off' });
  const count = h('span', { class: 'note' });
  let view;
  const filter = (e) => {
    if (kind.value && e.kind !== kind.value) return false;
    if (LEVELS[e.level] < LEVELS[level.value]) return false;
    const s = q.value.trim().toLowerCase();
    return !s || e.message.toLowerCase().includes(s) || (e.detail || []).some((d) => d.toLowerCase().includes(s));
  };
  const auto = btn('Autoscroll: on', () => {
    view.auto = !view.auto;
    sync();
    if (view.auto) view.el.scrollTop = view.el.scrollHeight;
  }, '', { 'aria-pressed': 'true' });
  const sync = () => {
    auto.textContent = `Autoscroll: ${view.auto ? 'on' : 'paused'}`;
    auto.setAttribute('aria-pressed', String(view.auto));
  };
  view = logView(sub, { filter, empty: 'No matching events.' });
  view.onchange = () => { count.textContent = `${view.shown} shown`; };
  view.onchange();
  view.el.addEventListener('wheel', () => { if (view.auto && view.el.scrollTop < view.el.scrollHeight - view.el.clientHeight - 40) { view.auto = false; sync(); } }, { passive: true });
  for (const c of [kind, level]) c.addEventListener('change', view.refresh);
  q.addEventListener('input', view.refresh);

  const clear = btn('Clear', async () => {
    if (await confirmBox('Clear the device event log?', { ok: 'Clear', danger: true })) await run(del('/api/events'), 'Event log cleared');
  }, 'warn');
  root.append(card('Event log',
    h('div', { class: 'toolbar' }, field('Kind', kind), field('Level', level), h('div', { class: 'f wide' }, h('span', null, 'Search'), q)),
    h('div', { class: 'row sp' }, h('div', { class: 'row' }, auto, count),
      h('div', { class: 'row' }, clear, h('a', { class: 'btn', href: '/api/events.txt', download: 'events.txt' }, 'Download'))),
    view.el));
  q.setAttribute('aria-label', 'Search events');
  loadEvents(`${kind.value ? 'kind=' + kind.value + '&' : ''}limit=200`).catch(() => {});
}

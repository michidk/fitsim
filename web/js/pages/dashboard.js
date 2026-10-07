// Dashboard: BLE status and connections, trainer (FTMS) status, live values, generator controls.
import { store } from '../store.js';
import { post, run } from '../api.js';
import { h, btn, card, METRICS, DEVICES, devStatus, fmt, hms, ago, lock, setVal, throttled, toast, confirmBox } from '../ui.js';
import { genEditor, modeLabel } from '../generator.js';

export function mount(root, sub) {
  const S = () => store.state;

  // ---- BLE summary
  const bleChips = DEVICES.map((d) => {
    const dot = h('i', { class: 'dot' }), txt = h('span');
    return { d, dot, txt, el: h('span', { class: 'chip' }, dot, h('b', null, d.label), txt) };
  });
  const clients = h('div');
  const bleCard = card('Bluetooth', h('div', { class: 'row' }, bleChips.map((c) => c.el)), clients);

  // ---- live tiles
  const tiles = {};
  const tileEls = METRICS.map((M) => {
    const v = h('span'), u = h('span', { class: 'u' }), s = h('div', { class: 's' });
    tiles[M.key] = { v, u, s, M };
    return h('div', { class: 'tile' }, h('div', { class: 'l' }, M.label), h('div', { class: 'v' }, v, u), s);
  });
  const liveCard = card('Live values', h('div', { class: 'tiles' }, tileEls));

  // ---- trainer (FTMS): status of the app-controlled trainer plus road simulation
  const keys = ['Control owner', 'Machine state', 'Last command', 'Target power', 'Target resistance', 'Commands received',
    'Grade', 'Wind', 'Crr', 'Cw'];
  const tv = Object.fromEntries(keys.map((k) => [k, h('dd')]));
  const trainerCard = card('Trainer (FTMS)',
    h('dl', { class: 'kv' }, keys.flatMap((k) => [h('dt', null, k), tv[k]])),
    h('p', { class: 'note' }, 'Grade, wind, Crr and Cw are sent by the app; display only, no metric changes.'),
    h('div', { class: 'row' },
      btn('Reset trainer state', async () => {
        if (await confirmBox('Clear the control owner, targets and simulation parameters?', { ok: 'Reset' })) run(post('/api/ftms/reset'), 'Trainer state reset');
      }, 'warn'),
      h('a', { href: '#/events?kind=ftms' }, 'Command log')));

  // ---- controls
  const rows = METRICS.map((M) => metricRow(M));
  const ctlCard = card('Controls', h('p', { class: 'note' }, 'Moving a slider switches that metric to manual mode.'), rows.map((r) => r.el));

  function metricRow(M) {
    const i0 = S().metrics[M.key];
    const slider = lock(h('input', { type: 'range', min: i0.min, max: i0.max, step: M.step, 'aria-label': M.label }));
    const num = lock(h('input', { type: 'number', min: i0.min, max: i0.max, step: M.step, 'aria-label': `${M.label} value` }));
    const unit = h('span', { class: 'unit' });
    const mode = h('span', { class: 'chip acc' }), src = h('span', { class: 'chip' });
    const send = throttled((v) => post('/api/state/' + M.slug, { value: v }).catch((e) => toast(e.message, 'err')), 100);
    slider.addEventListener('input', () => { num.value = slider.value; send(+slider.value); });
    slider.addEventListener('change', () => send.flush(+slider.value));
    num.addEventListener('change', () => {
      let v = parseFloat(num.value);
      if (!isFinite(v)) return;
      const m = S().metrics[M.key];
      v = Math.min(m.max, Math.max(m.min, v));
      num.value = slider.value = v;
      send.flush(v);
    });
    let editor = null;
    const more = btn('Mode...', () => {
      if (editor) return close();
      editor = genEditor(M, S().metrics[M.key], S().telemetry[M.key], close);
      el.append(editor);
      more.setAttribute('aria-expanded', 'true');
    }, 'sm', { 'aria-expanded': 'false' });
    function close() { editor?.remove(); editor = null; more.setAttribute('aria-expanded', 'false'); }
    const el = h('div', { class: 'mrow' },
      h('div', { class: 'nm' }, M.label), slider, num, unit,
      h('div', { class: 'meta' }, mode, src, more));
    function update(full) {
      const val = S().telemetry[M.key], info = S().metrics[M.key];
      setVal(slider, val); setVal(num, fmt(val, M.d));
      if (!full) return;
      unit.textContent = info.unit;
      mode.textContent = modeLabel(info.generator.mode);
      const over = info.source === 'trainer';
      src.hidden = info.source === 'generator';
      src.className = 'chip ' + (over ? 'warn' : '');
      src.textContent = over ? 'overridden by trainer (FTMS)' : `source: ${info.source}`;
      el.classList.toggle('over', over);
      slider.title = over ? "Overridden by the trainer; the slider sets this metric's manual value underneath" : '';
    }
    return { el, update };
  }

  // ---- updates
  function update() {
    const st = S(), t = st.telemetry;
    for (const M of METRICS) {
      const T = tiles[M.key];
      T.v.textContent = fmt(t[M.key], M.d);
      T.u.textContent = st.metrics[M.key].unit;
    }
    const tp = t.targetPower, ts = tiles.power.s;
    ts.replaceChildren();
    if (tp != null) ts.append('Target ', h('b', null, `${fmt(tp)} W`));
    const tr = t.targetResistance, rs = tiles.resistance.s;
    rs.textContent = tr != null ? `Target ${fmt(tr)} %` : '';
    rows.forEach((r) => r.update(false));
  }
  function updateState() {
    const st = S();
    for (const { d, dot, txt } of bleChips) {
      const [label, cls] = devStatus(st.ble?.devices?.[d.key]);
      dot.className = 'dot ' + cls;
      txt.textContent = label;
    }
    const cl = st.ble?.clients || [];
    clients.replaceChildren(cl.length ? h('div', { class: 'scroll-x' }, h('table', null,
      h('thead', null, h('tr', null, ['Connection', 'Connected', 'MTU', 'RSSI', 'Subscribed'].map((x) => h('th', null, x)))),
      h('tbody', null, cl.map((c) => h('tr', null, h('td', { class: 'mono' }, c.peer), h('td', null, hms((store.uptime - c.connectedMs) / 1000)),
        h('td', null, fmt(c.mtu)), h('td', null, c.rssi != null ? `${c.rssi} dBm` : '-'), h('td', null, (c.subscribed || []).join(', ') || '-'))))))
      : h('p', { class: 'note' }, 'No client connected.'));
    const t = st.trainer, lc = t.lastCommand, s = t.simulation;
    tv['Control owner'].textContent = t.controlOwner != null ? `handle ${t.controlOwner}` : 'none';
    tv['Machine state'].textContent = t.machineState;
    tv['Last command'].replaceChildren(...(lc ? [lc.summary || lc.name, ' ',
      h('span', { class: 'chip ' + (lc.result === 'success' ? 'ok' : 'bad') }, lc.result), ` ${ago(store.uptime - lc.t)}`] : ['-']));
    tv['Target power'].textContent = t.targetPower != null ? `${fmt(t.targetPower)} W` : '-';
    tv['Target resistance'].textContent = t.targetResistance != null ? `${fmt(t.targetResistance)} %` : '-';
    tv['Commands received'].textContent = t.commandsReceived;
    tv.Grade.textContent = s ? `${fmt(s.grade, 1)} %` : '-';
    tv.Wind.textContent = s ? `${fmt(s.windSpeed, 1)} m/s` : '-';
    tv.Crr.textContent = s ? fmt(s.crr, 4) : '-';
    tv.Cw.textContent = s ? `${fmt(s.cw, 2)} kg/m` : '-';
    rows.forEach((r) => r.update(true));
    update();
  }
  root.append(bleCard, trainerCard, liveCard, ctlCard);
  updateState();
  sub('state', updateState);
  sub('telemetry', update);
}

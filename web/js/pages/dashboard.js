// Dashboard: one card per metric (live value, slider, generator), then Bluetooth status and what the app sent.
import { store } from '../store.js';
import { post, run } from '../api.js';
import { h, btn, card, METRICS, DEVICES, devStatus, fmt, hms, ago, lock, setVal, throttled, toast, confirmBox } from '../ui.js';
import { genEditor, modeLabel } from '../generator.js';

// Readable names for the subscribed characteristics; they wrap where the API's camelCase names cannot.
const CHAR_LABELS = {
  indoorBikeData: 'Indoor Bike Data', machineStatus: 'Machine Status', controlPoint: 'Control Point',
  trainingStatus: 'Training Status', heartRateMeasurement: 'Heart Rate', cyclingPowerMeasurement: 'Cycling Power',
};
// FTMS target resistance is a level in 0.1 steps; the raw value 0..=100 is also the simulator's percent.
const level = (raw) => `${fmt(raw / 10, 1)} (${fmt(raw)} %)`;

export function mount(root, sub) {
  const S = () => store.state;

  // ---- metrics: the big value is also the number input
  const cards = METRICS.map((M) => metricCard(M));

  function metricCard(M) {
    const i0 = S().metrics[M.key];
    const num = lock(h('input', { type: 'number', class: 'val', min: i0.min, max: i0.max, step: M.step, 'aria-label': `${M.label} value` }));
    const slider = lock(h('input', { type: 'range', min: i0.min, max: i0.max, step: M.step, 'aria-label': M.label }));
    const unit = h('span', { class: 'u' });
    const target = h('div', { class: 's' });
    const mode = h('span', { class: 'chip acc' });
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
    function editMode() {
      const d = h('dialog', { class: 'dlg wide' });
      d.append(genEditor(M, S().metrics[M.key], S().telemetry[M.key], () => d.close()));
      d.addEventListener('close', () => d.remove());
      d.addEventListener('click', (e) => { if (e.target === d) d.close(); });
      document.body.append(d);
      d.showModal();
    }
    const el = h('section', { class: 'mcard', 'data-m': M.key },
      h('div', { class: 'l' }, M.label),
      h('div', { class: 'mv' }, num, unit),
      target, slider,
      h('div', { class: 'meta' }, mode, btn('Mode...', editMode, 'sm')));

    function update(full) {
      const st = S(), val = st.telemetry[M.key], info = st.metrics[M.key];
      setVal(slider, val); setVal(num, fmt(val, M.d));
      const t = M.key === 'power' ? st.telemetry.targetPower : M.key === 'resistance' ? st.telemetry.targetResistance : null;
      target.textContent = t != null ? `Target ${M.key === 'resistance' ? level(t) : `${fmt(t)} ${info.unit}`} from trainer` : '';
      if (!full) return;
      unit.textContent = info.unit;
      mode.textContent = modeLabel(info.generator.mode);
      const over = info.source === 'trainer';
      el.classList.toggle('over', over);
      slider.title = over ? "Overridden by the trainer; the slider sets this metric's manual value underneath" : '';
    }
    return { el, update };
  }

  // ---- Bluetooth: the three advertised devices and their connected clients
  const bleChips = DEVICES.map((d) => {
    const dot = h('i', { class: 'dot' }), txt = h('span');
    return { d, dot, txt, el: h('span', { class: 'chip' }, dot, h('b', null, d.label), txt) };
  });
  const advertised = h('p', { class: 'note' });
  const clients = h('div');
  const bleCard = card('Bluetooth', h('div', { class: 'row' }, bleChips.map((c) => c.el)), advertised, clients);

  // ---- from the app: what the connected app has asked the trainer to do (FTMS)
  const RX = [
    ['Target power', (t) => (t.targetPower != null ? `${fmt(t.targetPower)} W` : null)],
    ['Target resistance', (t) => (t.targetResistance != null ? level(t.targetResistance) : null)],
    ['Grade', (t) => (t.simulation ? `${fmt(t.simulation.grade, 2)} %` : null)],
    ['Wind', (t) => (t.simulation ? `${fmt(t.simulation.windSpeed, 3)} m/s` : null)],
    ['Crr', (t) => (t.simulation ? fmt(t.simulation.crr, 4) : null)],
    ['Cw', (t) => (t.simulation ? `${fmt(t.simulation.cw, 2)} kg/m` : null)],
  ];
  const rx = RX.map(([label, get]) => {
    const v = h('div', { class: 'v' });
    return { get, v, el: h('div', { class: 'rtile' }, h('div', { class: 'l' }, label), v) };
  });
  const keys = ['Control mode', 'Control owner', 'Machine state', 'Last command', 'Commands received'];
  const tv = Object.fromEntries(keys.map((k) => [k, h('dd')]));
  const rxNote = h('p', { class: 'note' });
  const appCard = card('From the app (FTMS)',
    rxNote,
    h('div', { class: 'rtiles' }, rx.map((r) => r.el)),
    h('dl', { class: 'kv' }, keys.flatMap((k) => [h('dt', null, k), tv[k]])),
    h('div', { class: 'row' },
      btn('Reset trainer state', async () => {
        if (await confirmBox('Clear the control owner, targets and simulation parameters?', { ok: 'Reset' })) run(post('/api/ftms/reset'), 'Trainer state reset');
      }, 'warn'),
      h('a', { href: '#/events?kind=ftms' }, 'Event log')));

  // ---- updates
  function update() { cards.forEach((c) => c.update(false)); }
  function updateState() {
    const st = S();
    for (const { d, dot, txt } of bleChips) {
      const [label, cls] = devStatus(st.ble?.devices?.[d.key]);
      dot.className = 'dot ' + cls;
      txt.textContent = label;
    }
    advertised.replaceChildren('Advertised as ', h('b', { class: 'mono' }, st.settings.deviceName), st.ble?.advertising ? '' : ' (not advertising)');
    const cl = st.ble?.clients || [];
    clients.replaceChildren(cl.length ? h('div', { class: 'scroll-x' }, h('table', null,
      h('thead', null, h('tr', null, ['Connection', 'Connected', 'MTU', 'RSSI', 'Subscribed', ''].map((x) => h('th', null, x)))),
      h('tbody', null, cl.map((c) => h('tr', null, h('td', { class: 'mono' }, c.peer), h('td', null, hms((store.uptime - c.connectedMs) / 1000)),
        h('td', null, fmt(c.mtu)), h('td', null, c.rssi != null ? `${c.rssi} dBm` : '-'), h('td', null, (c.subscribed || []).map((n) => CHAR_LABELS[n] || n).join(', ') || '-'),
        h('td', null, btn('Disconnect', () => run(post(`/api/ble/clients/${c.id}/disconnect`), 'Disconnect requested'), 'sm')))))))
      : h('p', { class: 'note' }, 'No client connected.'));
    const t = st.trainer, lc = t.lastCommand;
    const controlMode = t.targetPower != null ? 'ERG (target power)' : t.targetResistance != null ? 'Target resistance' : t.simulation ? 'Road simulation' : 'No target received';
    tv['Control mode'].textContent = controlMode;
    rxNote.textContent = t.simulation && t.targetPower == null && t.targetResistance == null
      ? 'The app controls resistance through grade, wind and road coefficients below. These parameters are recorded; the simulator does not calculate a resistance percentage from them.'
      : t.simulation
        ? 'Grade, wind and road coefficients show the last simulation parameters received. The current target takes precedence.'
        : 'Apps can send a power target, a resistance level, or road simulation parameters to control trainer resistance.';
    for (const r of rx) {
      const v = r.get(t);
      r.v.textContent = v ?? '-';
      r.el.classList.toggle('empty', v == null);
    }
    tv['Control owner'].textContent = t.controlOwner != null ? `handle ${t.controlOwner}` : 'none';
    tv['Machine state'].textContent = t.machineState;
    tv['Last command'].replaceChildren(...(lc ? [lc.summary || lc.name, ' ',
      h('span', { class: 'chip ' + (lc.result === 'success' ? 'ok' : 'bad') }, lc.result), ` ${ago(store.uptime - lc.t)}`] : ['-']));
    tv['Commands received'].textContent = t.commandsReceived;
    cards.forEach((c) => c.update(true));
    update();
  }
  root.append(h('div', { class: 'mcards' }, cards.map((c) => c.el)), h('div', { class: 'grid2 start' }, bleCard, appCard));
  updateState();
  sub('state', updateState);
  sub('telemetry', update);
}

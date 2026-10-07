// Generator ("Mode...") editor for one metric. PUT /api/metrics/{slug}.
import { h, btn, field, sw, toast } from './ui.js';
import { put } from './api.js';

const MODES = [
  ['fixed', 'Fixed'], ['manual', 'Manual'], ['ramp', 'Ramp'], ['oscillation', 'Oscillation'],
  ['randomVariation', 'Random variation'], ['randomWalk', 'Random walk'], ['sequence', 'Scripted sequence'],
];
export const modeLabel = (m) => (MODES.find((x) => x[0] === m) || [0, m])[1];

// [key, label, type] - type: undefined = number, 'bool', or an array of select options
const FIELDS = {
  fixed: [['value', 'Value']],
  manual: [['value', 'Value']],
  ramp: [['start', 'Start'], ['end', 'End'], ['durationS', 'Duration (s)'], ['repeat', 'Repeat', 'bool']],
  oscillation: [['center', 'Center'], ['amplitude', 'Amplitude'], ['periodS', 'Period (s)'], ['waveform', 'Waveform', ['sine', 'triangle', 'square', 'saw']]],
  randomVariation: [['base', 'Base'], ['variation', 'Variation (+/-)'], ['intervalS', 'Interval (s)']],
  randomWalk: [['min', 'Min'], ['max', 'Max'], ['maxStepPerS', 'Max step per second']],
};
const r2 = (v) => Math.round(v * 100) / 100;

function defaults(mode, v, m) {
  const span = m.max - m.min;
  switch (mode) {
    case 'fixed': case 'manual': return { value: r2(v) };
    case 'ramp': return { start: r2(v), end: r2(Math.min(m.max, v + span / 4)), durationS: 60, repeat: false };
    case 'oscillation': return { center: r2(v), amplitude: r2(span / 20), periodS: 20, waveform: 'sine' };
    case 'randomVariation': return { base: r2(v), variation: r2(span / 50), intervalS: 1 };
    case 'randomWalk': return { min: r2(Math.max(m.min, v - span / 10)), max: r2(Math.min(m.max, v + span / 10)), maxStepPerS: r2(span / 50) };
    case 'sequence': return { points: [{ t: 0, v: r2(v) }, { t: 10, v: r2(Math.min(m.max, v + span / 10)) }], interpolate: true, repeat: false };
    default: return {};
  }
}

/** Build the editor for `metric` ({key, slug, label}), pre-filled from info.generator. */
export function genEditor(metric, info, value, onDone) {
  const gen = info.generator;
  const sel = h('select', { onchange: () => build(sel.value) },
    ...MODES.map(([k, l]) => h('option', { value: k, selected: k === gen.mode }, l)));
  const body = h('div', { class: 'stack' });
  const err = h('div', { class: 'errbox', hidden: true });
  let read = () => ({ mode: sel.value });

  function build(mode) {
    err.hidden = true;
    const src = gen.mode === mode ? gen : defaults(mode, value || 0, info);
    if (mode === 'sequence') return buildSeq(src);
    const inputs = {};
    const fields = FIELDS[mode] || [];
    const grid = h('div', { class: 'fgrid' }, ...fields.map(([k, label, type]) => {
      if (type === 'bool') return (inputs[k] = sw(label, !!src[k]));
      if (Array.isArray(type)) {
        inputs[k] = h('select', null, ...type.map((o) => h('option', { value: o, selected: o === src[k] }, o)));
      } else inputs[k] = h('input', { type: 'number', step: 'any', value: src[k] });
      return field(label, inputs[k]);
    }));
    body.replaceChildren(grid);
    read = () => {
      const o = { mode };
      for (const [k, label, type] of fields) {
        const el = inputs[k];
        if (type === 'bool') o[k] = el.input.checked;
        else if (Array.isArray(type)) o[k] = el.value;
        else {
          o[k] = parseFloat(el.value);
          if (!isFinite(o[k])) throw new Error(`${label} must be a number`);
        }
      }
      return o;
    };
  }

  function buildSeq(src) {
    const rows = h('div', { class: 'stack' });
    const addBtn = btn('Add point', () => addRow(last() + 10, vlast()), 'sm');
    const rowEls = () => [...rows.querySelectorAll('.seqrow[data-r]')];
    const last = () => Math.max(0, ...rowEls().map((r) => +r._i[0].value || 0));
    const vlast = () => { const r = rowEls().pop(); return r ? r._i[1].value : 0; };
    function addRow(t, v) {
      if (rowEls().length >= 64) return toast('A sequence can have at most 64 points', 'err');
      const ti = h('input', { type: 'number', step: 'any', min: 0, value: t, 'aria-label': 'Time in seconds' });
      const vi = h('input', { type: 'number', step: 'any', value: v, 'aria-label': 'Value' });
      const r = h('div', { class: 'seqrow', 'data-r': '1' }, ti, vi, btn('Remove', () => r.remove(), 'sm', { 'aria-label': 'Remove point' }));
      r._i = [ti, vi];
      rows.append(r);
    }
    rows.append(h('div', { class: 'seqrow' }, h('small', null, 'Time (s)'), h('small', null, `Value (${info.unit})`), h('span')));
    src.points.forEach((p) => addRow(p.t, p.v));
    const interp = sw('Interpolate', !!src.interpolate, () => {});
    const rep = sw('Repeat', !!src.repeat, () => {});
    body.replaceChildren(rows, h('div', { class: 'row' }, addBtn, interp, rep));
    read = () => {
      const points = rowEls().map((r) => ({ t: parseFloat(r._i[0].value), v: parseFloat(r._i[1].value) }));
      if (!points.length) throw new Error('Add at least one point');
      points.forEach((p, i) => {
        if (!isFinite(p.t) || !isFinite(p.v)) throw new Error(`Point ${i + 1}: time and value must be numbers`);
        if (i && p.t < points[i - 1].t) throw new Error(`Point ${i + 1}: time must not go backwards`);
      });
      return { mode: 'sequence', points, interpolate: interp.input.checked, repeat: rep.input.checked };
    };
  }

  async function apply() {
    err.hidden = true;
    try {
      await put('/api/metrics/' + metric.slug, read());
      toast(`${metric.label}: generator applied`);
      onDone?.();
    } catch (e) {
      err.textContent = e.message;
      err.hidden = false;
    }
  }
  build(gen.mode);
  return h('div', { class: 'gen' },
    field(`${metric.label} mode`, sel), body, err,
    h('div', { class: 'row end' }, btn('Close', () => onDone?.()), btn('Apply', apply, 'pri')));
}

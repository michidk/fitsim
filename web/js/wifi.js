// Wi-Fi network picker shared by Settings and the provisioning screen.
import { get } from './api.js';
import { h, btn, field, spinner, toast, sw } from './ui.js';

export function wifiPicker() {
  const ssid = h('input', { type: 'text', maxLength: 32, autocomplete: 'off', spellcheck: false });
  const pass = h('input', { type: 'password', maxLength: 63, autocomplete: 'current-password' });
  const list = h('div', { class: 'netlist' });
  const status = h('small', { class: 'muted' });
  const scanBtn = btn('Scan networks', scan);
  let nets = [];

  function render() {
    list.replaceChildren(...nets.map((n) => {
      const b = h('button', { type: 'button', class: 'btn net', 'aria-pressed': String(ssid.value === n.ssid) },
        h('span', null, n.ssid), h('small', null, `${n.secure ? 'secured' : 'open'}  ${n.rssi} dBm`));
      b.onclick = () => { ssid.value = n.ssid; render(); if (n.secure) pass.focus(); };
      return b;
    }));
  }
  async function scan() {
    scanBtn.disabled = true;
    status.replaceChildren(spinner(), ' Scanning, this can take several seconds...');
    try {
      const r = await get('/api/wifi/scan');
      const best = new Map();
      for (const n of r.networks || []) if (n.ssid && (!best.has(n.ssid) || best.get(n.ssid).rssi < n.rssi)) best.set(n.ssid, n);
      nets = [...best.values()].sort((a, b) => b.rssi - a.rssi);
      status.textContent = nets.length ? `${nets.length} networks, tap one to select` : 'No networks found';
      render();
    } catch (e) {
      status.textContent = '';
      toast(e.message, 'err');
    }
    scanBtn.disabled = false;
  }
  ssid.addEventListener('input', render);
  const show = sw('Show password', false, (v) => { pass.type = v ? 'text' : 'password'; });

  /** Validated credentials; throws a readable message. */
  function creds() {
    const s = ssid.value.trim();
    if (!s) throw new Error('Enter or select a network name (SSID)');
    if (pass.value && pass.value.length < 8) throw new Error('A Wi-Fi password needs at least 8 characters (leave empty for an open network)');
    return { ssid: s, password: pass.value };
  }
  const el = h('div', { class: 'stack' },
    h('div', { class: 'row' }, scanBtn, status), list,
    field('Network name (SSID)', ssid), field('Password', pass), show);
  return { el, creds };
}

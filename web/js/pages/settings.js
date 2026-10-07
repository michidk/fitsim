// Settings: Wi-Fi, hostname, BLE name, firmware info, maintenance, help.
import { store } from '../store.js';
import { get, post, put, run, waitForDevice } from '../api.js';
import { h, btn, card, field, hms, confirmBox, overlay, setVal, dirtyTrack, clean, toast, ASCII } from '../ui.js';
import { wifiPicker } from '../wifi.js';

const kb = (n) => (n ? `${Math.round(n / 1024)} KB` : '-');

/** Show a "restarting" overlay until the device answers /api/system again. */
async function restarting(title, after = 3000) {
  const ov = overlay(title, 'Waiting for the device to come back. This usually takes 10 to 30 seconds...');
  const ok = await waitForDevice(after);
  if (ok) { ov.set('The device is back. Reloading...'); setTimeout(() => location.reload(), 600); }
  else {
    ov.set(`The device did not answer. If it joined another Wi-Fi network, connect to that network and open http://${store.state.system.hostname || 'fitness-simulator'}.local`);
    setTimeout(ov.close, 12000);
  }
}

export function mount(root, sub) {
  const S = () => store.state;

  // ---- Wi-Fi
  const picker = wifiPicker();
  const cur = h('dl', { class: 'kv' });
  const wifiCard = card('Wi-Fi', cur, picker.el, h('div', { class: 'row end' }, btn('Save and reboot', async () => {
    let c;
    try { c = picker.creds(); } catch (e) { return toast(e.message, 'err'); }
    if (!(await confirmBox(`Save "${c.ssid}" and reboot the device now? This page will reconnect when it is back.`, { ok: 'Save and reboot' }))) return;
    if (await run(post('/api/wifi', c))) restarting('Saved - the device is rebooting');
  }, 'pri')));

  // ---- hostname + BLE name
  const host = h('input', { type: 'text', maxLength: 32, autocomplete: 'off', spellcheck: false });
  const bleName = h('input', { type: 'text', maxLength: 24, autocomplete: 'off', spellcheck: false });
  const form = h('div', { class: 'stack' },
    field('Hostname', host, 'Lowercase letters, digits and hyphens. Applies after a reboot; the device is reachable as <hostname>.local'),
    field('Bluetooth device name', bleName, '1-24 printable ASCII characters. Applies immediately; reconnect or rescan in your app.'));
  dirtyTrack(form);
  const prefsCard = card('Preferences', form, h('div', { class: 'row end' }, btn('Save', async () => {
    const hostname = host.value.trim(), deviceName = bleName.value;
    if (!/^[a-z0-9-]{1,32}$/.test(hostname)) return toast('Hostname: 1-32 characters from a-z, 0-9 and -', 'err');
    if (!ASCII.test(deviceName)) return toast('Bluetooth name: 1-24 printable ASCII characters', 'err');
    if (await run(put('/api/settings', { hostname, deviceName }), hostname !== S().settings.hostname ? 'Saved - hostname applies after reboot' : 'Settings saved')) clean(form);
  }, 'pri')));

  // ---- firmware
  const info = Object.fromEntries(['Version', 'Chip', 'Uptime', 'Free heap', 'Total heap', 'Hostname', 'Wi-Fi mode', 'Address'].map((k) => [k, h('dd')]));
  let sys = S().system;
  const fwCard = card('Firmware', h('dl', { class: 'kv' }, Object.entries(info).flatMap(([k, v]) => [h('dt', null, k), v])),
    h('div', { class: 'row end' }, btn('Refresh', async () => { const r = await run(get('/api/system')); if (r) { sys = r; paint(); } }, 'sm')));
  function paint() {
    const s = sys, st = S().settings;
    info.Version.textContent = s.version;
    info.Chip.textContent = s.chip;
    info.Uptime.textContent = hms(store.uptime / 1000);
    info['Free heap'].textContent = kb(s.heapFree);
    info['Total heap'].textContent = kb(s.heapTotal);
    info.Hostname.textContent = s.hostname || st.hostname;
    info['Wi-Fi mode'].textContent = s.wifiMode;
    info.Address.textContent = s.ip || '-';
    const w = S().system;
    cur.replaceChildren(...[['Mode', w.wifiMode], ['Network', w.ssid || '-'], ['IP address', w.ip || '-'], ['Signal', w.rssi != null ? `${w.rssi} dBm` : '-']]
      .flatMap(([k, v]) => [h('dt', null, k), h('dd', null, v)]));
  }

  // ---- maintenance
  const act = (label, msg, path, danger, after, okText) => btn(label, async () => {
    if (await confirmBox(msg, { ok: okText || label, danger }) && await run(post(path))) after();
  }, danger ? 'danger' : 'warn');
  const maint = card('Maintenance',
    h('p', { class: 'note' }, 'Reboot restarts the firmware (settings are kept). Factory reset erases settings and Wi-Fi credentials.'),
    h('div', { class: 'row' },
      act('Reboot', 'Reboot the device now?', '/api/system/reboot', false, () => restarting('Rebooting')),
      act('Factory reset', 'Erase settings and Wi-Fi credentials, then reboot? This cannot be undone.', '/api/system/factory-reset', true, () => restarting('Factory reset in progress', 5000), 'Erase everything')));

  // ---- help
  const helpName = h('b', { class: 'mono' });
  const helpCard = card('Connect your app',
    h('p', null, 'Open the pairing screen of Zwift, MyWhoosh or your own app and search for the Bluetooth name below.'),
    h('p', null, helpName),
    h('p', null, 'This one device carries the trainer, heart-rate and power-meter services. Pick it in each role you need: Controllable trainer / Power source (power, cadence, speed, resistance and ERG control), Heart rate source, or Power source for a plain cycling power meter.'));

  function upd() {
    paint();
    helpName.textContent = S().settings.deviceName;
    setVal(host, S().settings.hostname);
    setVal(bleName, S().settings.deviceName);
  }
  root.append(h('div', { class: 'grid2' }, wifiCard, prefsCard), h('div', { class: 'grid2' }, fwCard, maint), helpCard);
  upd();
  sub('state', upd);
  sub('telemetry', () => { info.Uptime.textContent = hms(store.uptime / 1000); });
  get('/api/system').then((r) => { sys = r; paint(); }).catch(() => {});
}

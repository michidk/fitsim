// Provisioning mode: the device runs its own access point and needs Wi-Fi credentials.
import { store } from '../store.js';
import { post, run } from '../api.js';
import { h, btn, toast } from '../ui.js';
import { wifiPicker } from '../wifi.js';

export function mount(root) {
  const host = store.state.settings?.hostname || store.state.system?.hostname || 'fitness-simulator';
  const picker = wifiPicker();
  const card = h('section', { class: 'card setup' });
  const connect = btn('Connect', async () => {
    let c;
    try { c = picker.creds(); } catch (e) { return toast(e.message, 'err'); }
    connect.disabled = true;
    if (await run(post('/api/wifi', c))) {
      card.replaceChildren(
        h('h2', null, 'Saved'),
        h('p', null, `Saved - the device is rebooting, join your normal Wi-Fi and open http://${host}.local`),
        h('p', { class: 'note' }, 'If the name does not resolve, look up the device IP address in your router.'));
    } else connect.disabled = false;
  }, 'pri');
  card.append(
    h('h2', null, 'Wi-Fi setup'),
    h('p', null, 'This device is not connected to a Wi-Fi network yet, so it started its own access point. Choose the network it should join; it will save the settings and reboot.'),
    picker.el, h('div', { class: 'row end' }, connect));
  root.append(card);
}

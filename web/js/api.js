// fetch wrapper: JSON in/out, errors carry the server's `error` text.
import { toast } from './ui.js';

async function api(method, path, body, signal) {
  const r = await fetch(path, { method, signal, body: body === undefined ? undefined : JSON.stringify(body) });
  const txt = await r.text();
  let data = null;
  try { data = txt ? JSON.parse(txt) : null; } catch { /* non-JSON body */ }
  if (!r.ok) throw new Error((data && data.error) || `HTTP ${r.status}`);
  return data;
}
export const get = (p) => api('GET', p);
export const post = (p, b = {}) => api('POST', p, b);
export const put = (p, b) => api('PUT', p, b);
export const del = (p) => api('DELETE', p);

/** Await a request, toast success/error, resolve to the result or undefined on failure. */
export async function run(promise, okMsg) {
  try {
    const r = await promise;
    if (okMsg) toast(okMsg);
    return r ?? true;
  } catch (e) {
    toast(e.message, 'err');
    return undefined;
  }
}

/** Poll /api/system until the device answers again (after reboot / Wi-Fi change). */
export async function waitForDevice(firstDelay = 3000, maxMs = 90000) {
  const t0 = Date.now();
  await new Promise((r) => setTimeout(r, firstDelay));
  while (Date.now() - t0 < maxMs) {
    const ac = new AbortController();
    const to = setTimeout(() => ac.abort(), 2500);
    try { await api('GET', '/api/system', undefined, ac.signal); clearTimeout(to); return true; } catch { /* still down */ }
    clearTimeout(to);
    await new Promise((r) => setTimeout(r, 1500));
  }
  return false;
}

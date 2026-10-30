// Tabs, lane display mode (both kept in localStorage), the stale badge, 30 s refresh, SW.
const B = document.body, $ = s => document.querySelector(s), $$ = s => document.querySelectorAll(s);
const get = k => { try { return localStorage.getItem(k); } catch (e) { return null; } };
const put = (k, v) => { try { localStorage.setItem(k, v); } catch (e) {} };
function tab(t) {
  $$('main>section').forEach(s => s.hidden = s.id !== t);
  $$('nav a').forEach(a => a.classList.toggle('on', a.hash === '#' + t));
  put('tab', t);
}
$$('nav a').forEach(a => a.onclick = e => { e.preventDefault(); tab(a.hash.slice(1)); });
const t0 = get('tab');
tab(t0 && document.getElementById(t0) ? t0 : 'queue');
const MODES = ['glyph', 'ascii', 'pixel'];
function mode(m) { B.dataset.mode = m; $('#mode').textContent = m; put('mode', m); }
mode(MODES.includes(get('mode')) ? get('mode') : 'glyph');
$('#mode').onclick = () => mode(MODES[(MODES.indexOf(B.dataset.mode) + 1) % MODES.length]);
const ts = Number(B.dataset.ts) * 1000;
function net() {
  const stale = !navigator.onLine || Date.now() - ts > 90000;
  const hm = new Date(ts).toTimeString().slice(0, 5);
  $('#stale').hidden = !stale;
  $('#stale').textContent = 'stale · ' + hm;
  $$('form button, form select').forEach(b => b.disabled = stale);
}
net();
addEventListener('online', net);
addEventListener('offline', net);
setInterval(net, 5000);
if (location.search) history.replaceState(null, '', location.pathname + (location.search.includes('all=1') ? '?all=1' : ''));
setInterval(() => { if (!$('details[open]') && !$('select:focus')) location.reload(); }, 30000);
if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});

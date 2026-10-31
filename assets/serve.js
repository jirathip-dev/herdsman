// Tabs, lane display mode (both kept in localStorage), the live board (SSE `/live`, sync dot, ↻,
// pull to refresh), SW, the Lanes bottom sheet: card, transcript (5 s poll), reply box with
// one-time pairing, and blocked agents' menu buttons.
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
const MODES = ['horse', 'ascii', 'glyph'];
function mode(m) { B.dataset.mode = m; $('#mode').textContent = m; put('mode', m); }
mode(MODES.includes(get('mode')) ? get('mode') : 'horse');
$('#mode').onclick = () => mode(MODES[(MODES.indexOf(B.dataset.mode) + 1) % MODES.length]);
// Sync dot: g = live and up to date (pulses on an update), a = reconnecting (↻ spins), r = offline.
const dot = $('#dot'), rf = $('#rf');
let last = Number(B.dataset.ts) * 1000, es = null, down = Date.now(), tries = 0, poller = 0, busy = false;
function sync(s) {
  dot.className = s;
  rf.classList.toggle('spin', s === 'a' || busy);
  $('#stale').hidden = s !== 'r';
  $('#stale').textContent = 'last update ' + new Date(last).toTimeString().slice(0, 5);
  $$('form button, form select, .ask button').forEach(b => b.disabled = s === 'r');
}
function pulse() { dot.classList.remove('p'); void dot.offsetWidth; dot.classList.add('p'); }
const live = () => es && es.readyState === 1;
// Rows keep their open state across a swap (by agent, or heading + first cell); changed ones flash.
const head = e => { while ((e = e.previousElementSibling)) if (e.tagName === 'H2') return e.textContent; return ''; };
const key = e => e.dataset.agent || head(e) + (e.querySelector('summary>span') || e).textContent;
const sig = e => (e.querySelector('summary,.lr') || e).textContent.replace(/\b\d+[smhd]\b/g, '');
function frag(id, html) {
  const el = document.getElementById(id);
  if (!el) return;
  const rows = e => new Map([...e.querySelectorAll('details,.lane')].map(r => [key(r), r]));
  const old = rows(el);
  el.innerHTML = html;
  rows(el).forEach((r, k) => {
    const o = old.get(k);
    if (o && o.open) r.open = true;
    if (!o || sig(o) !== sig(r)) r.classList.add('fl');
  });
  last = Date.now();
}
// Network only (the SW passes `?frag=all` through); spins ↻ until done.
async function refresh() {
  busy = true;
  sync('a');
  try {
    const j = await (await fetch('/?frag=all', { cache: 'no-store' })).json();
    for (const id in j) frag(id, j[id]);
    busy = false;
    sync(live() ? 'g' : 'a');
    pulse();
  } catch (e) { busy = false; sync('r'); }
}
// Backoff 1, 2, 5, 10 s; after 30 s down, poll `?frag=all` every 10 s until SSE is back.
function connect() {
  if (es) es.close();
  try { es = new EventSource('live'); } catch (e) { es = null; return sync('r'); }
  es.onopen = () => { if (down) refresh(); down = 0; tries = 0; clearInterval(poller); poller = 0; sync('g'); };
  es.addEventListener('frag', e => { const j = JSON.parse(e.data); frag(j.id, j.html); sync('g'); pulse(); });
  es.onerror = () => {
    es.close();
    es = null;
    down = down || Date.now();
    sync(navigator.onLine ? 'a' : 'r');
    if (Date.now() - down > 30000 && !poller) poller = setInterval(refresh, 10000);
    setTimeout(() => es || connect(), 1000 * [1, 2, 5, 10][Math.min(tries++, 3)]);
  };
}
connect();
sync('a');
rf.onclick = () => { refresh(); if (!live()) { tries = 0; connect(); } };
document.addEventListener('visibilitychange', () => { if (!document.hidden) { down = down || Date.now(); tries = 0; connect(); } });
addEventListener('online', () => { tries = 0; connect(); });
addEventListener('offline', () => sync('r'));
if (location.search) history.replaceState(null, '', location.pathname + (location.search.includes('all=1') ? '?all=1' : ''));
B.insertAdjacentHTML('beforeend', `<div id=sheet hidden><div class=bd></div><div class=sh role=dialog aria-modal=true>
<div class=grip></div><div class=top></div><div class=chat></div>
<form class=pairf hidden><input name=code inputmode=numeric autocomplete=one-time-code maxlength=6 placeholder="6-digit code" aria-label="pairing code"><button>Pair</button>
<p class=m>First send from this device: run <b>herdsman serve pair</b> on the Mac and enter its code.</p></form>
<form class=reply><textarea name=text rows=1 maxlength=2000 placeholder=Message aria-label=message></textarea><button>Send</button></form>
<p class=st></p></div></div>`);
const S = $('#sheet'), sh = $('.sh'), chat = $('.chat'), st = $('.st');
let agent = null, act = '', timer = 0;
// A new bubble per turn marker: Claude's `⏺`, Hermes's tool lines, a typed prompt.
const MARK = { claude: l => l.startsWith('⏺'), hermes: l => l.includes('💻') || l.startsWith('┊') };
const typed = l => /^[❯>›]/.test(l);
async function poll() {
  try {
    const r = await fetch('/lane?agent=' + encodeURIComponent(agent));
    // Unpaired (401) or refused: only the row's 1-line activity, no transcript.
    if (!r.ok) return chat.replaceChildren(Object.assign(document.createElement('p'), { className: 'b', textContent: act }));
    const j = await r.json(), mark = MARK[j.harness] || (() => false);
    const end = chat.scrollTop + chat.clientHeight >= chat.scrollHeight - 8;
    chat.replaceChildren(...j.lines.reduce((bs, l) => {
      const t = l.trim();
      if (!bs.length || mark(t) || typed(t)) {
        bs.push(Object.assign(document.createElement('p'), { className: typed(t) ? 'b me' : 'b' }));
      }
      const b = bs[bs.length - 1];
      b.textContent += (b.textContent ? '\n' : '') + l;
      return bs;
    }, []));
    if (end) chat.scrollTop = chat.scrollHeight;
  } catch (e) { chat.textContent = 'no pane output'; }
}
function say(msg, ok) { st.textContent = msg; st.className = 'st ' + (ok ? 'g' : 'r'); }
function open(el) {
  agent = el.dataset.agent;
  act = el.querySelector('.act').textContent;
  $('.top').innerHTML = '<p><b></b></p>' + el.querySelector('.card').innerHTML;
  $('.top b').textContent = agent;
  const rev = el.dataset.role === 'rev';
  $('.reply').hidden = !!B.dataset.ro || rev;
  $('.pairf').hidden = true;
  chat.replaceChildren();
  say(rev ? 'reviewer lane: read-only' : '', true);
  sh.classList.remove('full');
  S.hidden = false;
  poll();
  timer = setInterval(poll, 5000);
}
function close() { S.hidden = true; clearInterval(timer); agent = null; }
// Delegated: rows and needs-you cards are swapped in place. A menu option POSTs `/answer`.
document.addEventListener('click', async e => {
  const b = e.target.closest('.lane .lr, .ask .pick, .ask .rp');
  if (!b) return;
  if (b.classList.contains('lr')) return open(b.parentNode);
  const a = b.closest('.ask'), lane = [...$$('.lane')].find(l => l.dataset.agent === a.dataset.ask);
  if (b.classList.contains('rp')) return lane && open(lane);
  // `h` names the menu this card shows: the server refuses a tap on a menu that has since changed.
  const r = await fetch('/answer', { method: 'POST', body: new URLSearchParams({ agent: a.dataset.ask, choice: b.value, h: b.dataset.h }) });
  const msg = (await r.text()).trim();
  // Re-render (fresh pane read) so stale buttons go at once; the message lands on the new card.
  await refresh();
  [a, ...[...$$('#needs .ask')].filter(c => c.dataset.ask === a.dataset.ask)].forEach(c => {
    const s = c.querySelector('.as');
    if (s) { s.textContent = msg; s.className = 'as ' + (r.ok ? 'g' : 'r'); }
  });
});
$('.bd').onclick = close;
addEventListener('keydown', e => { if (e.key === 'Escape' && !S.hidden) close(); });
// Drag the handle (or card): up = full height, down = back to 78 %, then closed.
let y0 = null;
[$('.grip'), $('.top')].forEach(h => {
  h.onpointerdown = e => { y0 = e.clientY; };
  h.onpointerup = e => {
    const dy = y0 === null ? 0 : e.clientY - y0;
    y0 = null;
    if (dy < -40) sh.classList.add('full');
    else if (dy > 60) sh.classList.contains('full') ? sh.classList.remove('full') : close();
  };
});
$('.reply').onsubmit = async e => {
  e.preventDefault();
  const r = await fetch('/send', { method: 'POST', body: new URLSearchParams({ agent, text: e.target.text.value }) });
  const msg = (await r.text()).trim();
  if (r.status === 401) $('.pairf').hidden = false;
  say(msg, r.ok);
  if (r.ok) { e.target.text.value = ''; setTimeout(poll, 1500); }
};
$('.pairf').onsubmit = async e => {
  e.preventDefault();
  const r = await fetch('/pair', { method: 'POST', body: new URLSearchParams({ code: e.target.code.value }) });
  say((await r.text()).trim(), r.ok);
  e.target.code.value = '';
  if (r.ok) { e.target.hidden = true; $('.reply').requestSubmit(); }
};
// Pull to refresh from the top (×0.5 resistance, past 70 px); never with the sheet open or a field
// focused; the axis locks after 10 px so a sideways swipe never pulls.
const M = $('main'), ptr = $('#ptr'), still = matchMedia('(prefers-reduced-motion: reduce)');
let p0 = null, py = 0, axis = '';
addEventListener('touchstart', e => {
  const typing = document.activeElement && document.activeElement.matches('input, textarea, select');
  p0 = scrollY <= 0 && S.hidden && !typing ? e.touches[0] : null;
  py = 0;
  axis = '';
}, { passive: true });
addEventListener('touchmove', e => {
  if (!p0) return;
  const dx = e.touches[0].clientX - p0.clientX, dy = e.touches[0].clientY - p0.clientY;
  if (!axis && Math.hypot(dx, dy) > 10) axis = dy > Math.abs(dx) ? 'y' : 'x';
  if (axis !== 'y') return;
  py = Math.max(0, dy * 0.5);
  M.style.transition = 'none';
  M.style.transform = `translateY(${py}px)`;
  ptr.hidden = false;
  ptr.style.transform = still.matches ? '' : `rotate(${py * 4}deg)`;
}, { passive: true });
addEventListener('touchend', () => {
  if (!p0) return;
  p0 = null;
  M.style.transition = M.style.transform = '';
  ptr.hidden = true;
  if (py > 70) refresh();
});
if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});

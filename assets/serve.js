// Tabs, lane display mode (both kept in localStorage), the live board (SSE `/live`, sync dot, ↻,
// pull to refresh), SW, the Lanes bottom sheet: card, Stop/Kill/Clean (`/act`), transcript per turn
// (5 s poll, load more), reply box with a `/` menu, Pair this phone, and blocked agents' menu buttons.
const B = document.body, $ = s => document.querySelector(s), $$ = s => document.querySelectorAll(s);
const get = k => { try { return localStorage.getItem(k); } catch (e) { return null; } };
const put = (k, v) => { try { localStorage.setItem(k, v); } catch (e) {} };
// URLs hang off R (`/` live, `` relative in a `serve --export`). #39 demo: static files, never a POST, never `/live`.
const R = B.dataset.root ?? '/', DEMO = !!B.dataset.demo;
if (DEMO) { const f = fetch; window.fetch = (u, o) => o && o.method === 'POST' ? Promise.reject(new Error('demo: no writes')) : f(u, o); }
function tab(t) {
  $$('main>section').forEach(s => s.hidden = s.id !== t);
  $$('nav a').forEach(a => a.classList.toggle('on', a.hash === '#' + t));
  put('tab', t);
  // #60: the live page leaves the Events body out; it loads when the tab opens.
  if (t === 'events' && !$('#events').childElementCount) part('frag?id=events').then(h => h && frag('events', h));
}
// #60: a part of the page read on demand (null when it cannot be read).
const part = src => fetch(R + src, { cache: 'no-store' }).then(r => r.ok ? r.text() : null).catch(() => null);
// A form POST of `data` (an object or FormData) to `path`.
const post = (path, data) => fetch(R + path, { method: 'POST', body: new URLSearchParams(data) });
// A `details[data-src]` row (a fleet's role forms) loads its body when opened, and again on each refresh.
async function load(d) {
  const h = await part(d.dataset.src);
  if (h === null) return;
  d.querySelectorAll(':scope>:not(summary)').forEach(n => n.remove());
  d.insertAdjacentHTML('beforeend', h);
  d.dataset.on = 1;
  tidy(d);
}
document.addEventListener('toggle', e => { if (e.target.matches('details[data-src]') && e.target.open) load(e.target); }, true);
$$('nav a').forEach(a => a.onclick = e => { e.preventDefault(); tab(a.hash.slice(1)); });
// A tapped alert opens `/?issue=N#tab`: that tab, the issue's row open.
const t0 = location.hash.slice(1) || get('tab'), DBG = new URLSearchParams(location.search), want = DBG.get('issue');
tab(t0 && document.getElementById(t0) ? t0 : 'queue');
$$('details.q').forEach(d => { if (want && d.querySelector('summary>span').textContent === '#' + want) { d.open = true; d.scrollIntoView(); } });
const MODES = ['horse', 'ascii', 'glyph'];
// The mode button sits in the Lanes tab (swapped with it): delegated, relabelled after each swap.
function mode(m) { B.dataset.mode = m; const b = $('#mode span') || $('#mode'); if (b) b.textContent = m; put('mode', m); }
mode(MODES.includes(get('mode')) ? get('mode') : 'horse');
document.addEventListener('click', e => { if (e.target.closest('#mode')) mode(MODES[(MODES.indexOf(B.dataset.mode) + 1) % MODES.length]); });
// Sync dot: g = live and up to date (pulses on an update), a = reconnecting (↻ spins), r = offline.
const dot = $('#dot'), rf = $('#rf');
let last = Number(B.dataset.ts) * 1000, es = null, down = Date.now(), tries = 0, poller = 0, busy = false;
function sync(s) {
  if (DEMO) s = 'demo';
  dot.className = s;
  rf.classList.toggle('spin', s === 'a' || busy);
  $('#stale').hidden = s !== 'r';
  $('#stale').textContent = 'last update ' + new Date(last).toTimeString().slice(0, 5);
  $$('form button, form select, .ask button').forEach(b => b.disabled = s === 'r' || DEMO);
  if (DEMO) $$('input, textarea, .acts button').forEach(b => b.disabled = true);
}
function pulse() { dot.classList.remove('p'); void dot.offsetWidth; dot.classList.add('p'); }
const live = () => es && es.readyState === 1;
// Rows keep their open state across a swap (by agent, or heading + first cell); changed ones flash.
const head = e => { while ((e = e.previousElementSibling)) if (e.tagName === 'H2') return e.textContent; return ''; };
const key = e => e.dataset.agent || head(e) + (e.querySelector('summary>span') || e).textContent;
const sig = e => (e.querySelector('summary,.lr') || e).textContent.replace(/\b\d+[smhd]\b/g, '');
// #65: an update patches the live tree to the new one: rows (`details`, `.lane`) matched by key(), the rest by
// position; equal nodes are left alone, `open` is the user's. So the page keeps its scroll, open rows and focus.
const rk = n => n.nodeType === 1 && n.matches('details,.lane') ? key(n) : '';
// Give `a` the attributes of `b` (also herd.js's horses), but the ones in `keep`, which stay `a`'s own.
function attrs(a, b, keep = []) {
  for (const { name } of [...a.attributes]) if (!b.hasAttribute(name) && !keep.includes(name)) a.removeAttribute(name);
  for (const { name, value } of b.attributes) if (a.getAttribute(name) !== value) a.setAttribute(name, value);
}
function morph(a, b) {
  if (a.nodeType !== 1) { if (a.data !== b.data) a.data = b.data; return; }
  if (a.isEqualNode(b)) return;
  attrs(a, b, ['open', 'data-on']);
  if (a.dataset.on) return; // a loaded part keeps its body; refresh() re-reads it
  const rows = new Map(), rest = [], kids = [...b.childNodes];
  for (const c of a.childNodes) { const k = rk(c); if (k && !rows.has(k)) rows.set(k, c); else if (!k) rest.push(c); }
  kids.forEach((n, i) => {
    const k = rk(n), j = k ? -1 : rest.findIndex(c => c.nodeName === n.nodeName);
    let o = k ? rows.get(k) : rest[j];
    if (k) rows.delete(k); else if (j >= 0) rest.splice(j, 1);
    if (o) morph(o, n); else o = n;
    if (a.childNodes[i] !== o) a.insertBefore(o, a.childNodes[i] || null);
  });
  while (a.childNodes.length > kids.length) a.lastChild.remove();
}
function frag(id, html) {
  const el = document.getElementById(id);
  if (!el) return;
  const rows = e => new Map([...e.querySelectorAll('details,.lane')].map(r => [key(r), r]));
  const old = new Map([...rows(el)].map(([k, r]) => [k, sig(r)])), nu = el.cloneNode(false), y = scrollY;
  nu.innerHTML = html;
  tidy(nu);
  morph(el, nu);
  if (scrollY !== y) scrollTo(0, y);
  rows(el).forEach((r, k) => { if (old.get(k) !== sig(r)) r.classList.add('fl'); });
  last = Date.now();
  alerts();
  if (id === 'lanes') mode(B.dataset.mode);
}
// #68: buttons hold labels, never sentences: a control's runtime result is an 11 px `p.st` under it (red on error).
function note(b, msg, ok) {
  let s = b.nextElementSibling;
  if (!s || !s.matches('p.st')) b.after(s = Object.assign(document.createElement('p'), { className: 'st' }));
  s.textContent = msg;
  s.classList.toggle('r', !ok);
}
// Enable alerts: Home Screen app only (iOS allows push there alone); subscribe with the VAPID key, POST it.
// Unpaired (any 401): `[enable alerts]` dims, "pair this phone first" under it; kept across live swaps.
const standalone = matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
let nopair = false;
function alerts() {
  const b = $('#push');
  if (!b) return;
  b.hidden = !standalone || !('PushManager' in window);
  if (nopair) { b.disabled = true; note(b, 'pair this phone first', false); }
}
// Also shows the sheet's Pair button (open() hides it again).
function unpaired() { nopair = true; alerts(); $('.pairf').hidden = false; }
alerts();
document.addEventListener('click', async e => {
  const b = e.target.closest('#push');
  if (!b) return;
  try {
    if (await Notification.requestPermission() !== 'granted') throw new Error('notifications are off for this app');
    const key = Uint8Array.from(atob(b.dataset.key.replace(/-/g, '+').replace(/_/g, '/')), c => c.charCodeAt(0));
    const reg = await navigator.serviceWorker.ready;
    const s = (await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key })).toJSON();
    const r = await post('push/subscribe', { endpoint: s.endpoint, p256dh: s.keys.p256dh, auth: s.keys.auth });
    const msg = (await r.text()).trim();
    if (r.status === 401) return unpaired();
    note(b, msg, r.ok);
    if (r.ok) refresh();
  } catch (err) { note(b, 'alerts failed: ' + err.message, false); }
});
// Network only (the SW passes `frags` through); spins ↻ until done.
async function refresh() {
  if (DEMO) return herdPull();
  claim();
  busy = true;
  sync('a');
  try {
    const j = await (await fetch(R + 'frags', { cache: 'no-store' })).json();
    for (const id in j) frag(id, j[id]);
    $$('details[data-src][open]').forEach(load);
    herdPull();
    busy = false;
    sync(live() ? 'g' : 'a');
    pulse();
  } catch (e) { busy = false; sync('r'); }
}
// Backoff 1, 2, 5, 10 s; after 30 s down, poll `frags` every 10 s until SSE is back.
function connect() {
  if (DEMO) return;
  if (es) es.close();
  try { es = new EventSource('live'); } catch (e) { es = null; return sync('r'); }
  // After a gap (the first open included: a cached page may have painted) the dot is refresh()'s: amber until `frags` lands.
  es.onopen = () => { if (down) refresh(); else sync('g'); down = 0; tries = 0; clearInterval(poller); poller = 0; };
  es.addEventListener('frag', e => { const j = JSON.parse(e.data); frag(j.id, j.html); sync('g'); pulse(); claim(); });
  es.addEventListener('herd', e => herdData(JSON.parse(e.data)));
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
if (location.search) history.replaceState(null, '', location.pathname + (location.search.includes('all=1') ? '?all=1' : '') + location.hash);
// #66: an action's result is a one-line toast under the header: ok fades after 4 s, an error stays; a tap dismisses it.
function toast(text) {
  $$('.msg.g, .msg.r').forEach(m => m.remove());
  const m = document.createElement('p'), ok = !/^(REFUSED|error)/.test(text);
  m.className = 'msg ' + (ok ? 'g' : 'r');
  m.textContent = text;
  m.onclick = () => m.remove();
  $('header').after(m);
  if (ok) setTimeout(() => { m.classList.add('out'); setTimeout(() => m.remove(), 400); }, 4000);
}
if ($('.msg.g, .msg.r')) toast($('.msg.g, .msg.r').textContent);
// ▲/▼ and Set POST with fetch (same checks), toast the redirect's `msg` and patch in place: the scroll stays (#65).
// The plain form POST is the no-JS fallback.
document.addEventListener('submit', async e => {
  const f = e.target, op = f.elements.op && f.elements.op.value;
  if (op !== 'mv' && op !== 'set') return;
  e.preventDefault();
  try {
    const r = await post('', new FormData(f));
    toast(r.redirected ? new URL(r.url).searchParams.get('msg') || '' : 'error: ' + (await r.text()).trim());
    refresh();
  } catch (err) { toast('error: ' + err.message); }
});
B.insertAdjacentHTML('beforeend', `<div id=sheet hidden><div class=bd></div><div class=sh role=dialog aria-modal=true>
<div class=hd><p><b></b></p></div><div class=top></div><div class=acts><button type=button data-op=stop>Stop</button><button type=button data-op=kill>Kill</button><button type=button data-op=clean>Clean</button></div>
<div class=cf hidden><p></p><button type=button class=yes>Kill</button><button type=button class=no>Cancel</button></div>
<div class=chat><button type=button class=more hidden>Load more</button><div class=old></div><div class=now></div></div>
<p class=pairf hidden><button type=button class=pairb>Pair this phone</button></p>
<ul class=cmds hidden></ul>
<form class=reply><textarea name=text rows=1 maxlength=2000 placeholder="Message, or / for commands" aria-label=message></textarea><button>Send</button></form>
<p class=st></p></div></div>`);
if (DEMO) sync();
const S = $('#sheet'), sh = $('.sh'), chat = $('.chat'), st = $('.st'), now = $('.now'), old = $('.old'), more = $('.more');
let agent = null, timer = 0, cur = 0, y = 0;
// The Lanes row of `agent` (its sheet card), from the needs-you cards and the herd.
const laneOf = agent => [...$$('.lane')].find(l => l.dataset.agent === agent);
const el = (tag, cls, text) => Object.assign(document.createElement(tag), { className: cls, textContent: text || '' });
// #58: controls are bracket text, the label in a span that ellipsises (#68). A select hides behind its short `[opus ▾]` label (the native picker
// keeps full names); ▲/▼ drop their words; a choice (ask or menu option) wraps in a span, its full text in title (#63 A).
const short = o => (o ? o.textContent : '').replace(/^default.*/, 'default').replace(/.*\b(opus|sonnet|haiku)\b.*/, '$1');
function tidy(root) {
  root.querySelectorAll('select').forEach(s => {
    if (s.parentNode.matches('.pk')) return;
    const k = el('label', 'pk'), t = el('span', '');
    s.replaceWith(k);
    k.append(t, s);
    const show = () => { t.textContent = short(s.selectedOptions[0]) + ' ▾'; k.title = s.selectedOptions[0] ? s.selectedOptions[0].textContent : ''; };
    s.addEventListener('change', show);
    show();
  });
  for (const b of root.querySelectorAll('button')) {
    const m = b.textContent.match(/^([▲▼]) (up|down)$/);
    if (m) { b.textContent = m[1]; b.setAttribute('aria-label', 'move ' + m[2]); }
    if (b.matches('.lr, .hh, .hqh') || (b.childNodes.length === 1 && b.firstChild.nodeName === 'SPAN')) continue;
    const t = el('span', '');
    t.append(...b.childNodes);
    b.replaceChildren(t);
    if (b.matches('[name=choice], .pick')) b.title = b.textContent;
  }
}
tidy(document);
rf.innerHTML = '<i>↻</i>';
// One block per turn (server-grouped): the typed prompt, text runs, tool calls collapsed to their head line.
const render = turns => turns.map(t => {
  const d = el('div', 'turn');
  if (t.me) d.append(el('p', 'b me', t.me));
  for (const i of t.items) {
    if (i.tool === undefined) { d.append(el('p', 'b', i.text)); continue; }
    const x = el('details', 'tool');
    x.append(el('summary', '', i.tool));
    if (i.body) x.append(el('pre', '', i.body));
    d.append(x);
  }
  return d;
});
const lane = q => fetch(DEMO ? 'lane/' + encodeURIComponent(agent) + '.json' : R + 'lane?agent=' + encodeURIComponent(agent) + '&lines=200' + q);
async function poll() {
  try {
    const r = await lane('');
    // Unpaired (401): say so, with the Pair button; never a silent one-liner.
    if (r.status === 401) {
      unpaired();
      return now.replaceChildren(el('p', 'unp', 'Pair this phone to see the conversation'));
    }
    if (!r.ok) return now.replaceChildren(el('p', 'm', (await r.text()).trim()));
    const j = await r.json(), end = chat.scrollTop + chat.clientHeight >= chat.scrollHeight - 8;
    const open = new Set([...now.querySelectorAll('details[open] summary')].map(s => s.textContent));
    now.replaceChildren(...render(j.turns));
    now.querySelectorAll('summary').forEach(s => { if (open.has(s.textContent)) s.parentNode.open = true; });
    if (!old.childElementCount) { cur = j.before; more.hidden = !j.more; }
    if (end) chat.scrollTop = chat.scrollHeight;
  } catch (e) { now.textContent = 'no pane output'; }
}
// Older lines from the pane's scrollback, kept above the live part.
more.onclick = async () => {
  const r = await lane('&before=' + cur);
  if (!r.ok) return;
  const j = await r.json(), h = chat.scrollHeight;
  old.prepend(...render(j.turns));
  cur = j.before;
  more.hidden = !j.more;
  chat.scrollTop += chat.scrollHeight - h;
};
function say(msg, ok) { st.textContent = msg; st.className = 'st ' + (ok ? 'g' : 'r'); }
// Stop/Kill/Clean call `/act`; kill and clean dry-run first (kill shows what is lost, clean why it is unsafe).
const act = (op, confirm) => post('act', { agent, op, confirm: confirm ? '1' : '' });
async function run(op) {
  const r = await act(op, op === 'stop'), msg = (await r.text()).trim();
  if (r.status === 401) unpaired();
  if (op === 'stop' || !r.ok) return say(msg, r.ok);
  $('.cf p').textContent = msg.split('; ').find(l => !l.startsWith('dry run')) || msg;
  $('.cf .yes span').textContent = op === 'kill' ? 'Kill' : 'Clean';
  $('.cf').dataset.op = op;
  $('.cf').hidden = false;
}
$('.acts').onclick = e => { const b = e.target.closest('button'); if (b) run(b.dataset.op); };
$('.cf .no').onclick = () => { $('.cf').hidden = true; };
$('.cf .yes').onclick = async () => {
  const r = await act($('.cf').dataset.op, true);
  $('.cf').hidden = true;
  say((await r.text()).trim(), r.ok);
  refresh();
};
async function cleanable() {
  const b = $('[data-op=clean]'), r = await act('clean', false), msg = (await r.text()).trim();
  b.disabled = !r.ok;
  b.title = r.ok ? '' : msg;
  if (!r.ok && r.status !== 401 && !st.textContent) say('clean: ' + msg.split(': ').pop(), false);
}
function open(e) {
  agent = e.dataset.agent;
  const top = $('.top');
  top.innerHTML = e.querySelector('.card').innerHTML;
  top.scrollTop = 0;
  $('.hd b').textContent = agent;
  const rev = e.dataset.role === 'rev';
  $('.reply').hidden = !!B.dataset.ro || rev;
  $('.acts').hidden = !!B.dataset.ro;
  $$('.pairf, .cf, .cmds').forEach(x => x.hidden = true);
  now.replaceChildren();
  old.replaceChildren();
  say(rev ? 'reviewer lane: read-only' : '', true);
  // Lock the page under the sheet (iOS scrolls the body otherwise); reopened mid-close, it is still locked.
  if (shut) { clearTimeout(shut); shut = 0; S.classList.remove('out'); } else {
    y = scrollY;
    B.style.top = -y + 'px';
    document.documentElement.classList.add('lock');
    S.hidden = false;
    S.classList.add('drag');
    pos(sh.offsetHeight, 0);
    void sh.offsetHeight;
    S.classList.remove('drag');
  }
  fit();
  snap(kb() ? 'full' : 'mid');
  poll();
  if (!B.dataset.ro && !DEMO) cleanable();
  timer = setInterval(poll, 5000);
}
// Slides off-screen, then hides; the scroll lock and the page position come back after the slide.
function close() {
  if (S.hidden || shut) return;
  clearInterval(timer);
  agent = null;
  d0 = null;
  S.classList.remove('drag');
  S.classList.add('out');
  pos(sh.offsetHeight);
  shut = setTimeout(() => {
    shut = 0;
    S.hidden = true;
    S.classList.remove('out');
    document.documentElement.classList.remove('lock');
    B.style.top = '';
    scrollTo(0, y);
  }, 220);
}
// #70: the sheet is a full-height box placed by translateY at a detent (`full` stops under the safe area).
// pos() moves it, fades the backdrop with it, and pads its bottom by the hidden part so the composer shows.
const DET = { full: 1, mid: .78, half: .5 }, bd = $('.bd');
let at = 'mid', ty = 0, shut = 0, padT = 0, d0 = null, moves = [], to = null, rq = 0;
const off = d => Math.max(0, S.clientHeight * (1 - DET[d]) - sh.offsetTop);
function pos(t, pad) {
  ty = t;
  sh.style.transform = `translateY(${t}px)`;
  const h = sh.offsetHeight, half = off('half');
  bd.style.opacity = Math.min(1, Math.max(0, (h - t) / (h - half || 1)));
  if (pad !== undefined) sh.style.paddingBottom = `calc(${pad}px + env(safe-area-inset-bottom))`;
}
// Animate to a detent; the composer stays on screen (pad by the larger offset, then the detent's once settled).
function snap(d) {
  at = d;
  sh.classList.toggle('full', d === 'full');
  const t = off(d);
  pos(t, Math.max(t, ty));
  clearTimeout(padT);
  padT = setTimeout(() => pos(t, t), 300);
}
// The sheet follows the visual viewport: the composer stays above the keyboard and returns when it closes.
// While the keyboard is up (a field in the sheet focused) the sheet is full and does not drag.
const vv = window.visualViewport;
const kb = () => !!sh.querySelector('input:focus, textarea:focus') || !!vv && vv.height < innerHeight * .75;
function fit() {
  if (vv) { S.style.top = vv.offsetTop + 'px'; S.style.height = vv.height + 'px'; }
  if (!S.hidden && !shut && !d0) snap(kb() ? 'full' : at);
}
if (vv) { vv.addEventListener('resize', fit); vv.addEventListener('scroll', fit); }
sh.addEventListener('focusin', fit);
sh.addEventListener('focusout', () => setTimeout(fit, 100));
// `/` first in the reply box: a filtered command list (tap or Enter); stop/kill/clean act, the rest go as text.
const CMDS = ['/stop', '/kill', '/clean', '/compact', '/clear', '/status'], ta = $('.reply textarea'), cmds = $('.cmds');
const matches = () => /^\/\S*$/.test(ta.value) ? CMDS.filter(c => c.startsWith(ta.value)) : [];
function pick(c) {
  cmds.hidden = true;
  if (['/stop', '/kill', '/clean'].includes(c)) { ta.value = ''; return run(c.slice(1)); }
  ta.value = c;
  $('.reply').requestSubmit();
}
ta.oninput = () => {
  const m = matches();
  cmds.replaceChildren(...m.map(c => el('li', '', c)));
  cmds.hidden = !m.length;
};
ta.onkeydown = e => { if (e.key === 'Enter' && !e.shiftKey && matches().length) { e.preventDefault(); pick(matches()[0]); } };
cmds.onclick = e => { const li = e.target.closest('li'); if (li) pick(li.textContent); };
// Delegated: rows and needs-you cards are swapped in place. A menu option POSTs `/answer`.
document.addEventListener('click', async e => {
  const b = e.target.closest('.lane .lr, .ask .pick, .ask .rp');
  if (!b) return;
  if (b.classList.contains('lr')) return open(b.parentNode);
  const a = b.closest('.ask'), lane = laneOf(a.dataset.ask);
  if (b.classList.contains('rp')) return lane && open(lane);
  // `h` names the menu this card shows: the server refuses a tap on a menu that has since changed.
  const r = await post('answer', { agent: a.dataset.ask, choice: b.value, h: b.dataset.h });
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
// #70: the sheet follows the finger 1:1 (light rubber band above full), one move per frame. On release: a flick
// (over 0.5 px/ms in the last 80 ms) goes one detent its way, else the nearest; below half, or flicked down from it, closes.
function grab(y) {
  if (kb() || shut) return false;
  clearTimeout(padT);
  d0 = { y, t: ty };
  moves = [];
  S.classList.add('drag');
  return true;
}
function drag(y, ts) {
  const raw = d0.t + y - d0.y;
  to = raw < 0 ? raw / 3 : raw;
  moves.push([y, ts]);
  if (!rq) rq = requestAnimationFrame(() => { rq = 0; if (to !== null) pos(to); });
}
function drop() {
  cancelAnimationFrame(rq);
  rq = 0;
  if (to !== null) pos(to);
  to = d0 = null;
  S.classList.remove('drag');
  const now = performance.now(), m = moves.filter(([, t]) => now - t <= 80), a = m[0], b = m[m.length - 1];
  const v = m.length > 1 && b[1] > a[1] ? (b[0] - a[0]) / (b[1] - a[1]) : 0, ds = ['full', 'mid', 'half'], os = ds.map(off);
  let i;
  if (Math.abs(v) > .5) i = v > 0 ? os.findIndex(o => o > ty + 1) : Math.max(0, os.findLastIndex(o => o < ty - 1));
  else i = ty > os[2] + 16 ? -1 : os.reduce((k, o, j) => Math.abs(o - ty) < Math.abs(os[k] - ty) ? j : k, 0);
  if (i < 0) close(); else snap(ds[i]);
}
const hd = $('.hd');
hd.onpointerdown = e => { if (grab(e.clientY)) try { hd.setPointerCapture(e.pointerId); } catch (_) {} };
hd.onpointermove = e => { if (d0) drag(e.clientY, e.timeStamp); };
hd.onpointerup = hd.onpointercancel = () => { if (d0) drop(); };
// The conversation scrolls; pulled down while at its top (or once it reaches it), it hands the touch to the sheet.
// Move and end listen on the touched node itself: a poll may swap it out mid-touch, and then nothing bubbles.
chat.addEventListener('touchstart', e => {
  const t = e.target;
  let c0 = e.touches[0].clientY;
  const move = e => {
    const y = e.touches[0].clientY;
    if (!d0 && (chat.scrollTop > 0 || y <= c0 || !e.cancelable || !grab(c0))) { c0 = y; return; }
    e.preventDefault();
    drag(y, e.timeStamp);
  };
  const end = () => {
    t.removeEventListener('touchmove', move);
    t.removeEventListener('touchend', end);
    t.removeEventListener('touchcancel', end);
    if (d0) drop();
  };
  t.addEventListener('touchmove', move, { passive: false });
  t.addEventListener('touchend', end);
  t.addEventListener('touchcancel', end);
}, { passive: true });
$('.reply').onsubmit = async e => {
  e.preventDefault();
  const r = await post('send', { agent, text: e.target.text.value });
  const msg = (await r.text()).trim();
  if (r.status === 401) unpaired();
  say(msg, r.ok);
  if (r.ok) { e.target.text.value = ''; setTimeout(poll, 1500); }
};
// #59: Pair this phone (header, blocked cards, the sheet): POST `pair/request` shows a code; its 10 min HttpOnly cookie is this
// browser's alone. The Mac approves it (`serve approve`, logged: the live Events frag arrives, or ↻), then `pair/claim` keeps it a year.
// The panel flows under the header (or the sheet's Pair button), never over a card; a tap closes it; it expires with the code.
B.insertAdjacentHTML('beforeend', '<div id=pairp class=pairp hidden></div>');
const pp = $('#pairp');
let pairing = false, ttl;
async function claim() {
  if (!pairing) return;
  pairing = false;
  const r = await fetch(R + 'pair/claim', { method: 'POST' }).catch(() => null);
  if (!r || !r.ok) { pairing = true; return; }
  nopair = false;
  clearTimeout(ttl);
  pp.className = 'pairp g';
  pp.replaceChildren(el('p', '', (await r.text()).trim()));
  $$('.pairb').forEach(b => b.hidden = true);
  setTimeout(() => { pp.hidden = true; }, 4000);
  connect();
  refresh();
  if (agent) poll();
}
document.addEventListener('click', async e => {
  if (!e.target.closest('.pairb')) { if (e.target.closest('#pairp')) { pairing = false; clearTimeout(ttl); pp.hidden = true; } return; }
  const r = await fetch(R + 'pair/request', { method: 'POST' }).catch(err => ({ ok: false, text: () => 'error: ' + err.message })), t = (await r.text()).trim();
  pairing = r.ok;
  pp.className = 'pairp' + (r.ok ? '' : ' r');
  if (!e.target.closest('#pairp')) (e.target.closest('.pairf') || $('header')).after(pp);
  pp.hidden = false;
  clearTimeout(ttl);
  if (r.ok) pp.replaceChildren(el('p', 'code', t), el('p', 'm', `Approve on the Mac: herdsman serve approve ${t} · or tell the conductor 'pair ${t}'`));
  else pp.replaceChildren(el('p', '', t));
  pp.scrollIntoView({ block: 'nearest' });
  if (!r.ok) return;
  ttl = setTimeout(() => { pairing = false; pp.className = 'pairp r'; pp.replaceChildren(el('span', '', 'code expired — '), $('.pairb').cloneNode(true)); }, 6e5);
});
// Pull to refresh from the top (×0.5 resistance, past 70 px); never with the sheet open or a field
// focused; the axis locks after 10 px so a sideways swipe never pulls. In Herd mode: the current page.
const M = $('main'), ptr = $('#ptr'), still = matchMedia('(prefers-reduced-motion: reduce)');
let p0 = null, py = 0, axis = '';
addEventListener('touchstart', e => {
  const typing = document.activeElement && document.activeElement.matches('input, textarea, select');
  const hp = herdPage();
  p0 = (hp ? hp.scrollTop : scrollY) <= 0 && S.hidden && !typing ? e.touches[0] : null;
  py = 0;
  axis = '';
}, { passive: true });
addEventListener('touchmove', e => {
  if (!p0) return;
  const dx = e.touches[0].clientX - p0.clientX, dy = e.touches[0].clientY - p0.clientY;
  if (!axis && Math.hypot(dx, dy) > 10) axis = dy > Math.abs(dx) ? 'y' : 'x';
  if (axis !== 'y') return;
  py = Math.max(0, dy * 0.5);
  const box = herdPage() || M;
  box.style.transition = 'none';
  box.style.transform = `translateY(${py}px)`;
  ptr.hidden = false;
  ptr.style.transform = still.matches ? '' : `rotate(${py * 4}deg)`;
}, { passive: true });
addEventListener('touchend', () => {
  if (!p0) return;
  p0 = null;
  [M, ...$$('.hp')].forEach(b => { b.style.transition = b.style.transform = ''; });
  ptr.hidden = true;
  if (py > 70) refresh();
});
if ('serviceWorker' in navigator) navigator.serviceWorker.register(R + 'sw.js').catch(() => {});

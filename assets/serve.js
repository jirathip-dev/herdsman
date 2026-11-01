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
function unpaired() { nopair = true; alerts(); $('.pairf').hidden = false; $$('[data-op=close]').forEach(b => b.hidden = true); }
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
  const f = e.target, op = f.elements.op && f.elements.op.value, c = e.submitter && e.submitter.dataset.confirm;
  if (c && !confirm(c)) return e.preventDefault(); // #130: merge anyway
  if (op !== 'mv' && op !== 'set') return;
  e.preventDefault();
  try {
    const r = await post('', new FormData(f));
    toast(r.redirected ? new URL(r.url).searchParams.get('msg') || '' : 'error: ' + (await r.text()).trim());
    refresh();
  } catch (err) { toast('error: ' + err.message); }
});
B.insertAdjacentHTML('beforeend', `<div id=sheet hidden><div class=bd></div><div class=sh role=dialog aria-modal=true>
<div class=hd><p><b></b></p></div><div class=top></div><div class=acts><button type=button data-op=stop>Stop</button><button type=button data-op=kill>Kill</button><button type=button data-op=clean>Clean</button><button type=button data-op=close>Close pane</button></div>
<div class=cf hidden><p></p><button type=button class=yes>Kill</button><button type=button class=no>Cancel</button></div>
<div class=chat><button type=button class=more hidden>Load more</button><div class=now></div></div>
<p class=live hidden></p>
<p class=pairf hidden><button type=button class=pairb>Pair this phone</button></p>
<ul class=cmds hidden></ul>
<form class=reply><textarea name=text rows=1 maxlength=2000 placeholder="Message, or / for commands" aria-label=message></textarea><button>Send</button></form>
<p class=st></p></div></div>`);
if (DEMO) sync();
const S = $('#sheet'), sh = $('.sh'), chat = $('.chat'), st = $('.st'), now = $('.now'), more = $('.more');
let agent = null, timer = 0, y = 0, nopane = false, opane = null;
// The Lanes row of `agent` (its sheet card), from the needs-you cards and the herd.
const laneOf = agent => [...$$('.lane')].find(l => l.dataset.agent === agent);
const el = (tag, cls, text) => Object.assign(document.createElement(tag), { className: cls, textContent: text || '' });
// #58: controls are bracket text, the label in a span that ellipsises (#68). A select hides behind its short `[opus ▾]` label (the native picker
// keeps full names); ▲/▼ drop their words; a choice (ask or menu option) wraps in a span, its full text in title (#63 A).
const short = o => (o ? o.textContent : '').replace(/^default.*/, 'default').replace(/.*\b(opus|sonnet|haiku)\b.*/, '$1');
// #115: one map of what each state word means: the [?] legend sheet and every state word's title.
const HINT = { queued: 'waiting its turn', impl: 'being built', 'review(n)': 'being checked, round n',
  'fix(n)': 'fixing review findings, round n', ready: 'passed, waiting to merge', merged: 'merged into staging',
  parked: 'lane closed, its work saved on the PR', dead: 'lane gone with unsaved work', 'needs you': 'waiting for your answer',
  working: 'its lane is running', idle: 'its lane waits for a next step', done: 'its lane finished its turn' };
const word = t => { const w = t.trim().split(/[\s·]/)[0].replace(/\(\d+\)/, '(n)'); return /^(needs-human|blocked|⏸)/.test(w) ? 'needs you' : w; };
const hint = t => HINT[word(t)] ? `${word(t)} = ${HINT[word(t)]}` : '';
const tip = e => { const h = hint(e.textContent); if (h) e.title = h; };
// #115: a fleet's rows as a `git log --graph`: a row blocked by a row in view hangs off it (│ ├─ └─), children in queue
// order, depth-first; past depth 3 it clamps (┆─, `on #b`), so the gutter is at most 6 columns. A blocker out of view is
// a `! #b` tag, a --serial issue the lock and its fleet's label. The card sits in a box; ▲/▼ move among siblings.
function graph(root) {
  for (const p of new Set([...root.querySelectorAll('details.q')].map(d => d.parentNode))) {
    let run = [];
    for (const c of [...p.children, null]) {
      if (c && c.matches('details.q') && !c.querySelector('.gt')) { run.push(c); continue; }
      if (run.length) tree(p, run, c);
      run = [];
    }
  }
}
function tree(p, rows, end) {
  const n = d => d.querySelector('summary>span').textContent.slice(1), by = new Map(rows.map(d => [n(d), d]));
  const up = d => by.get(d.dataset.b) || null, kids = new Map(rows.map(d => [d, []])), roots = [];
  // A blocker loop (a → b → a) draws its rows as roots.
  const par = d => { let x = up(d); const seen = new Set([d]); while (x && !seen.has(x)) { seen.add(x); x = up(x); } return x ? null : up(d); };
  rows.forEach(d => (par(d) ? kids.get(par(d)) : roots).push(d));
  const walk = (list, pre, depth) => list.forEach((d, i) => {
    const last = i === list.length - 1, deep = depth > 3, st = d.querySelector('summary>span:nth-of-type(2)'), s = st.textContent;
    const dot = st.className === 'r' || /^(⏸|needs)/.test(s) ? '!' : s === 'ready' ? '✓' : /^(impl|review|fix)/.test(s) ? '●' : '○';
    const elbow = !depth ? '' : deep ? '<i class=c>┆─</i>' : last ? '<i class=e>└─</i>' : '<i class=t>├─</i>';
    const k = kids.get(d).length && depth < 3 ? ' k' : '';
    d.classList.add('d' + Math.min(depth, 3));
    d.querySelector('summary').insertAdjacentHTML('afterbegin', `<i class=gt>${pre.slice(0, 2).join('')}${elbow}<b class="${st.className}${k}">${dot}</b></i>`);
    const w = el('span', '');
    st.replaceWith(w);
    w.append(st);
    tip(st);
    const tag = (cls, text, title = '') => { const t = el('span', cls, text); if (title) { t.title = title; t.setAttribute('aria-label', title); } w.append(' ', t); };
    if (d.dataset.s) tag('tag m', '🔒' + d.dataset.s, 'serial: one lane at a time');
    if (d.dataset.b && !par(d)) tag('tag r', '! #' + d.dataset.b);
    if (deep) tag('m', 'on #' + d.dataset.b);
    const cd = el('div', 'cd');
    cd.append(...[...d.children].filter(c => c.tagName !== 'SUMMARY'));
    d.append(cd);
    // The parser lifts the ▲/▼ forms out of their `<p><span class=btns>`; they go back in one `.btns` line.
    const sib = list.filter(x => 'q' in x.dataset), j = sib.indexOf(d), old = cd.querySelectorAll('form:has([name=before],[name=after])');
    const mv = (label, key, to) => { const c = old[0].cloneNode(true), i = c.querySelector('[name=before],[name=after]'); i.name = key; i.value = n(to); c.querySelector('button').textContent = label; return c; };
    const fs = old.length ? [j > 0 && mv('▲ up', 'before', sib[j - 1]), sib[j + 1] && mv('▼ down', 'after', sib[j + 1])].filter(Boolean) : [];
    cd.querySelectorAll('.btns').forEach(b => b.parentNode.remove());
    old.forEach(f => f.remove());
    if (fs.length) { const line = el('p', 'btns'); line.append(...fs); cd.append(line); }
    p.insertBefore(d, end);
    walk(kids.get(d), depth ? [...pre, last ? '<i>  </i>' : '<i class=p>│ </i>'] : [], depth + 1);
  });
  walk(roots, [], 0);
}
function tidy(root) {
  graph(root);
  root.querySelectorAll('.lr>span:nth-of-type(3)').forEach(tip);
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
// #138 chat blocks: pure from here to `chat:end` (tests/chat_blocks.rs runs it under node). A role run is a block:
// you (a typed prompt), the assistant (prose), a tool run (`⏺ Name(…)`/`•`/`$`/`┊` plus its `⎿`/indented output),
// status (a line all in grey: `Ran 2 shell commands`), and the gap line where the rolling transcript missed lines.
const GAP = '┄ not captured: scrolled past between reads ┄';
const esc = s => s.replace(/[&<>"']/g, c => `&#${c.charCodeAt(0)};`);
const plain = l => l.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '');
// A terminal colour to the nearest board token by hue (no terminal hex reaches the page); grey is the default text
// when light, `m` when dark. Bands: red < 12° or ≥ 330°, amber < 70°, green < 160°, cyan < 200°, blue < 255°, else purple.
function tone(r, g, b) {
  const mx = Math.max(r, g, b), d = mx - Math.min(r, g, b);
  if (d < 40) return mx >= 200 ? '' : 'm';
  const h = (60 * (mx === r ? (g - b) / d : mx === g ? (b - r) / d + 2 : (r - g) / d + 4) + 360) % 360;
  return h < 12 || h >= 330 ? 'r' : h < 70 ? 'a' : h < 160 ? 'g' : h < 200 ? 'c' : h < 255 ? 'l' : 'p';
}
const BASIC = ['m', 'r', 'g', 'a', 'l', 'p', 'c', ''], CUBE = [0, 95, 135, 175, 215, 255];
const rgb256 = n => n < 232 ? [CUBE[(n - 16) / 36 | 0], CUBE[((n - 16) / 6 | 0) % 6], CUBE[(n - 16) % 6]] : Array(3).fill(8 + 10 * (n - 232));
// An ANSI line as [text, classes] runs: fg `t-<token>`, bold `t-b`, dim `t-m`, and red/green backgrounds (diffs)
// as `bg-r`/`bg-g`, `…2` when bright (changed words).
function runs(line) {
  const out = []; let fg = '', bg = '', b = false, dim = false;
  const col = (c, i) => c[i] === 5 ? [rgb256(c[i + 1]), i + 1] : [c.slice(i + 1, i + 4), i + 3];
  const tint = ([r, g, bl]) => { const t = tone(r, g, bl), x = c => c[0] === c[1] && c[1] === c[2] ? '' : c; return /^[rg]$/.test(t) ? `bg-${t}${Math.max(r, g, bl) > 65 ? '2' : ''}` : x([r, g, bl]) && ''; };
  for (const [i, part] of line.split('\x1b[').entries()) {
    const m = i ? /^([0-9;?]*)([A-Za-z])/.exec(part) : null;
    if (m && m[2] === 'm') {
      const c = m[1].split(';').map(Number);
      for (let j = 0; j < c.length; j++) {
        const n = c[j];
        if (n === 0) { fg = bg = ''; b = dim = false; } else if (n === 1) b = true; else if (n === 2) dim = true;
        else if (n === 22) b = dim = false; else if (n === 39) fg = ''; else if (n === 49) bg = '';
        else if (n >= 30 && n <= 37 || n >= 90 && n <= 97) fg = BASIC[n % 10];
        else if (n === 41 || n === 42) bg = n === 41 ? 'bg-r' : 'bg-g';
        else if (n === 38 || n === 48) {
          const [v, k] = c[j + 1] === 5 && c[j + 2] < 16 ? [null, j + 2] : col(c, j + 1);
          if (n === 38) fg = v ? tone(...v) : BASIC[c[j + 2] % 8]; else bg = v ? tint(v) : '';
          j = k;
        }
      }
    }
    const t = m ? part.slice(m[0].length) : part;
    const cls = [fg ? 't-' + fg : dim ? 't-m' : '', b ? 't-b' : '', bg].filter(Boolean).join(' ');
    if (t && out.length && out[out.length - 1][1] === cls) out[out.length - 1][0] += t; else if (t) out.push([t, cls]);
  }
  return out;
}
const span = ([t, c], md) => {
  if (md && c === 't-l') return `<code>${esc(t)}</code>`;
  const h = md ? esc(t).replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>').replace(/`([^`]+)`/g, '<code>$1</code>') : esc(t);
  return c ? `<span class="${c}">${h}</span>` : h;
};
// Prose (md): Claude's code runs (blue) and raw markdown's `code` become chips, `**bold**` bold; text is escaped first.
const html = (rs, md) => rs.map(r => span(r, md)).join('');
// Drop the first n characters of runs.
const cut = (rs, n) => rs.flatMap(([t, c]) => { const k = Math.min(n, t.length); n -= k; return k < t.length ? [[t.slice(k), c]] : []; });
const muted = rs => rs.some(([t]) => t.trim()) && rs.every(([t, c]) => !t.trim() || /\bt-m\b/.test(c));
const pad = p => p.length - p.trimStart().length;
// Lines to role runs: {k: me | ai | run | qs | gap, head, lines}.
function group(lines) {
  const out = []; let cur = null;
  const add = (k, l) => out.push(cur = { k, head: l, lines: [], ind: pad(plain(l)) });
  for (const l of lines) {
    const p = plain(l), t = p.trim(), n = pad(p);
    if (!t) { if (cur && /me|ai|run/.test(cur.k)) cur.lines.push(l); }
    else if (t === GAP) add('gap', l);
    else if (/^[❯›>]\s/.test(t)) add('me', l);
    else if (/^[⏺•]\s*\w+\(/.test(t) || /^(\$|┊|💻)\s/.test(t)) add('run', l);
    else if (cur && cur.k === 'run' && (t[0] === '⎿' || n > cur.ind + 2)) cur.lines.push(l);
    else if (muted(runs(l))) add('qs', l);
    else if (cur && !/^[⏺•]/.test(t) && (cur.k === 'ai' || cur.k === 'me' && n > cur.ind)) cur.lines.push(l);
    else add('ai', l);
  }
  return out;
}
// Minimal markdown on prose: paragraphs (wrapped lines joined), `#` headings, `-`/`1.` lists.
function prose(lines) {
  const items = []; let last = null;
  lines.forEach((l, i) => {
    const rs0 = runs(l), lead = /^\s*([⏺❯›>•]\s*)?/.exec(plain(l))[0].length, rs = cut(rs0, i ? pad(plain(l)) : lead), t = rs.map(r => r[0]).join('');
    let m;
    if (!t.trim()) last = null;
    else if ((m = /^#{1,6}\s+/.exec(t))) { items.push({ t: 'h', h: [html(cut(rs, m[0].length), true)] }); last = null; }
    else if ((m = /^([-*•]|\d+[.)])\s+/.exec(t))) {
      const tag = /\d/.test(m[1]) ? 'ol' : 'ul', end = items[items.length - 1];
      last = end && end.t === tag ? end : (items.push({ t: tag, h: [] }), items[items.length - 1]);
      last.h.push(html(cut(rs, m[0].length), true));
    } else if (last) last.h[last.h.length - 1] += ' ' + html(rs, true);
    else items.push(last = { t: 'p', h: [html(rs, true)] });
  });
  return items.map(x => x.t === 'p' ? `<p>${x.h[0]}</p>` : x.t === 'h' ? `<p class=h>${x.h[0]}</p>` : `<${x.t}>${x.h.map(h => `<li>${h}</li>`).join('')}</${x.t}>`).join('');
}
// A tool run's output: `⎿` and the output indent dropped, each line a hanging indent under its own start (past a diff
// gutter `52 -`); over 20 lines, the first 20, or on a failure the first and last 10 around `⋯ N lines`.
function output(lines, fail) {
  while (lines.length && !plain(lines[lines.length - 1]).trim()) lines = lines.slice(0, -1);
  const n0 = lines.length ? /^[\s⎿]*/.exec(plain(lines[0]))[0].length : 0;
  const row = l => {
    let rs = runs(l); rs = cut(rs, Math.min(n0, /^[\s⎿]*/.exec(plain(l))[0].length));
    const t = rs.map(r => r[0]).join(''), bg = (rs.find(r => /bg-[rg]\b/.test(r[1])) || ['', ''])[1].match(/bg-[rg]\b/);
    return `<span class="ol${bg ? ' ' + bg[0] : ''}" style="--i:${/^(\s*\d+ [-+ ]?)?\s*/.exec(t)[0].length}">${html(rs) || ' '}</span>`;
  };
  const n = lines.length, all = lines.map(row);
  if (n <= 20) return `<div class=out><pre>${all.join('')}</pre></div>`;
  fail = fail || lines.some(l => /FAILED|error/.test(plain(l)));
  const [a, b] = fail ? [10, n - 10] : [20, n];
  return `<div class="out cap"><pre>${all.slice(0, a).join('')}<span class=rest>${all.slice(a, b).join('')}</span>` +
    (fail ? `<span class="ol cut t-m">⋯ ${n - 20} lines</span>${all.slice(b).join('')}` : '') +
    `</pre><p class=more2><button type=button><span>show all</span></button><span class=m>20 of ${n} lines</span></p></div>`;
}
// Blocks as {key, k, html, open}: tool runs start collapsed, you and the assistant open (Q4); status is a quiet line.
// `busy`: the live status line shows activity, so a last tool run with no output yet is `running`.
function blocks(lines, harness, busy) {
  const gs = group(lines);
  return gs.map((g, i) => {
    const head = plain(g.head).trim(), key = g.k + ':' + head + ':' + plain(g.lines[0] || '').trim();
    if (g.k === 'qs' || g.k === 'gap') return { key, k: g.k, html: `<p class="qs${g.k === 'gap' ? ' gap' : ''}">${html(cut(runs(g.head), pad(plain(g.head))))}</p>` };
    const sum = (k, rest) => `<summary><span class=k>${k}</span>${rest}<i class=cv></i></summary>`;
    if (g.k !== 'run') {
      const body = [g.head, ...g.lines], pv = esc(head.replace(/^[⏺❯›>•]\s*(#+\s+)?/, ''));
      return { key, k: g.k, open: true, html: `<details class="blk ${g.k}">${sum(g.k === 'me' ? 'you' : esc(harness || 'agent'), `<span class=pv>${pv}</span>`)}<div class=md>${prose(body)}</div></details>` };
    }
    const m = /^(?:[⏺•]\s*(\w+)\((.*?)\)?(\s+·.*)?|(?:┊\s*)?(?:💻\s*)?(\$|terminal)?\s*(.*))$/.exec(head);
    const name = m[1] || (m[4] ? '$' : ''), shell = !name || /^(\$|Bash|Shell|terminal)$/.test(name);
    const cmd = m[1] ? m[2] : m[5], text = g.lines.map(plain).join('\n'), dot = runs(g.head).find(r => r[0].trim());
    const fail = !!dot && /\bt-r\b/.test(dot[1]), code = /exit code (\d+)/i.exec(text);
    const tag = fail ? `<span class=t-r>${code ? 'exit ' + code[1] : 'failed'}</span>` : busy && i === gs.length - 1 && !text.trim() ? '<span class=t-a>running</span>' : '';
    return { key, k: 'run', html: `<details class="blk run">${sum(shell ? '$' : esc(name), `<span class=cmd>${esc(cmd)}</span>${tag}`)}${text.trim() ? output(g.lines, fail) : ''}</details>` };
  });
}
// The longest suffix of a that is a prefix of b (as text): two reads' overlap.
const overlap = (a, b) => { for (let k = Math.min(a.length, b.length); k > 0; k--) if (a.slice(-k).every((x, i) => plain(x) === plain(b[i]))) return k; return 0; };
// chat:end
const lane = q => fetch(DEMO ? 'lane/' + encodeURIComponent(agent) + '.json' : R + 'lane?agent=' + encodeURIComponent(agent) + '&lines=200' + q);
// The settled lines seen (merged across reads by their overlap) plus the live block, drawn as blocks: a block whose key
// and HTML are unchanged keeps its node (no scroll jump, #65), new lines land in the last block; open state per lane.
let hist = [], tail = [], nlive = 0, harness = '', spin = false;
const store = () => { try { return JSON.parse(sessionStorage.getItem('chat:' + agent)) || {}; } catch (e) { return {}; } };
function paint() {
  const seen = new Map([...now.children].map(n => [n.dataset.k, n])), keep = store(), end = chat.scrollTop + chat.clientHeight >= chat.scrollHeight - 8;
  now.replaceChildren(...blocks(hist.concat(tail), harness, spin).map(b => {
    const n = seen.get(b.key);
    if (n && n.dataset.h === b.html) return n;
    const t = document.createElement('template');
    t.innerHTML = b.html;
    const x = t.content.firstChild;
    Object.assign(x.dataset, { k: b.key, h: b.html });
    if (x.tagName === 'DETAILS') x.open = keep[b.key] ?? (n ? n.open : !!b.open);
    return x;
  }));
  if (end) chat.scrollTop = chat.scrollHeight;
}
async function poll() {
  // #83: a lane with no pane has no transcript to read.
  if (nopane) return now.replaceChildren(el('p', 'm', 'no pane: the transcript closed with it'));
  try {
    const r = await lane('');
    // Unpaired (401): say so, with the Pair button; never a silent one-liner.
    if (r.status === 401) {
      unpaired();
      return now.replaceChildren(el('p', 'unp', 'Pair this phone to see the conversation'));
    }
    if (!r.ok) return now.replaceChildren(el('p', 'm', (await r.text()).trim()));
    const j = await r.json(), done = j.lines.slice(0, j.lines.length - j.live), k = overlap(hist, done);
    opane = j.pane;
    if (!hist.length) more.hidden = !j.more;
    hist = k || !hist.length || !done.length ? hist.concat(done.slice(k)) : done;
    [tail, nlive, harness, spin] = [j.lines.slice(done.length), j.live, j.harness, !!j.status];
    $('.live').hidden = !j.status;
    if (j.status) $('.live').innerHTML = `<b>${esc([...j.status][0])}</b>${esc(j.status.slice([...j.status][0].length))}`;
    paint();
  } catch (e) { now.textContent = 'no pane output'; }
}
// Older lines from the rolling transcript, joined above the seen ones by their overlap.
more.onclick = async () => {
  const r = await lane('&before=' + Math.max(0, hist.length + nlive - 20));
  if (!r.ok) return;
  const j = await r.json(), h = chat.scrollHeight;
  hist = j.lines.slice(0, j.lines.length - overlap(j.lines, hist)).concat(hist);
  more.hidden = !j.more;
  paint();
  chat.scrollTop += chat.scrollHeight - h;
};
chat.addEventListener('toggle', e => {
  const k = e.target.dataset && e.target.dataset.k, s = store();
  if (!k) return;
  s[k] = e.target.open;
  try { sessionStorage.setItem('chat:' + agent, JSON.stringify(s)); } catch (err) {}
}, true);
// [show all] / [show less] on a capped tool output.
chat.addEventListener('click', e => {
  const b = e.target.closest('.more2 button'), o = b && b.closest('.out');
  if (o) b.firstChild.textContent = o.classList.toggle('all') ? 'show less' : 'show all';
});
// `ok` null: a neutral note, e.g. why a control is disabled (#83).
function say(msg, ok) { st.textContent = msg; st.className = 'st ' + (ok == null ? 'm' : ok ? 'g' : 'r'); }
// Stop/Kill/Clean call `/act`; kill and clean dry-run first (kill shows what is lost, clean why it is unsafe).
// #95: an Others agent (not a lane) gets only Close pane, confirmed here before `/act` closes it.
const act = (op, confirm) => post('act', { agent: op === 'close' && opane || agent, op, confirm: confirm ? '1' : '' });
async function run(op) {
  const r = op === 'close' ? null : await act(op, op === 'stop');
  const msg = r ? (await r.text()).trim() : `close ${agent}'s pane? its worktree and files stay`;
  if (r && r.status === 401) unpaired();
  if (op === 'stop' || (r && !r.ok)) return say(msg, r.ok);
  $('.cf p').textContent = msg.split('; ').find(l => !l.startsWith('dry run')) || msg;
  $('.cf .yes span').textContent = { kill: 'Kill', clean: 'Clean', close: 'Close' }[op];
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
  if (!r.ok && r.status !== 401 && !st.textContent) say('clean: ' + msg.split(': ').pop(), null);
}
function open(e) {
  agent = e.dataset.agent;
  opane = null;
  const top = $('.top');
  top.innerHTML = e.querySelector('.card').innerHTML;
  top.scrollTop = 0;
  $('.hd b').textContent = agent;
  const rev = e.dataset.role === 'rev';
  $('.reply').hidden = !!B.dataset.ro || rev;
  $('.acts').hidden = !!B.dataset.ro;
  // #83: a lane with no pane has nothing to stop or kill; #95: an Others agent is no lane, only its pane closes.
  nopane = 'gone' in e.dataset;
  const other = e.dataset.role === 'other';
  $$('[data-op=stop], [data-op=kill]').forEach(b => b.hidden = nopane || other);
  $('[data-op=clean]').hidden = other;
  $('[data-op=close]').hidden = !other || nopair;
  $$('.pairf, .cf, .cmds').forEach(x => x.hidden = true);
  now.replaceChildren();
  hist = []; tail = [];
  $('.live').hidden = true;
  say(rev ? 'reviewer lane: read-only' : other ? 'not a herdsman lane: herdsman can only close its pane' : '', other ? null : true);
  sh.classList.remove('lg');
  show(kb() ? 'full' : 'mid');
  poll();
  if (!other && !B.dataset.ro && !DEMO) cleanable();
  timer = setInterval(poll, 5000);
}
// #115 amendment A: [?] shows the legend in the same sheet, its lane parts hidden (`.lg`): a line per state from HINT,
// the dots, and each fleet's serial mark with the paths it guards (none without serial_paths).
function legend() {
  agent = null;
  sh.classList.add('lg');
  $('.hd b').textContent = 'what the marks mean';
  const serial = new Set([...$$('#queue [data-sp]')].map(f => `🔒${f.dataset.s} one at a time: touches ${f.dataset.sp}`));
  const dots = ['● a lane is on it', '○ waiting', '✓ ready', '! needs you'];
  $('.top').replaceChildren(...Object.entries(HINT).map(([w, h]) => el('p', '', `${w} = ${h}`)), ...[...dots, ...serial].map(t => el('p', '', t)));
  show('mid');
}
// Lock the page under the sheet (iOS scrolls the body otherwise); reopened mid-close, it is still locked.
function show(d) {
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
  snap(d);
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
// #127: a prompt's command expands on tap; `auto-denies in m:ss` counts down from the pane read.
document.addEventListener('click', e => { const c = e.target.closest('.ask .command'); if (c) c.classList.toggle('open'); });
setInterval(() => $$('.ask .cd').forEach(c => {
  const s = Math.max(0, c.dataset.until - Math.floor(Date.now() / 1000));
  c.textContent = `auto-denies in ${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}), 1000);
document.addEventListener('click', async e => {
  if (e.target.closest('.qhelp')) return legend();
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
  const r = await fetch(R + 'pair/request', { method: 'POST' }).catch(err => ({ ok: false, text: () => 'error: ' + err.message })), s = (await r.text()).trim(), [t, h] = s.split('\n');
  pairing = r.ok;
  pp.className = 'pairp' + (r.ok ? '' : ' r');
  if (!e.target.closest('#pairp')) (e.target.closest('.pairf') || $('header')).after(pp);
  pp.hidden = false;
  clearTimeout(ttl);
  if (r.ok) pp.replaceChildren(el('p', 'code', t), el('p', 'm', `Approve on the Mac: herdsman serve approve ${t}` + (h ? ` · ${h}` : '')));
  else pp.replaceChildren(el('p', '', s));
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
// #114: iOS Safari ignores user-scalable=no for pinch.
addEventListener('gesturestart', e => e.preventDefault(), { passive: false });
if ('serviceWorker' in navigator) navigator.serviceWorker.register(R + 'sw.js').catch(() => {});

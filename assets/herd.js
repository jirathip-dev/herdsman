// #37 Herd mode: Corral's Herd screen (jirathip-dev/corral ios/FleetNotifier/UI/Herd: RanchEnvironment,
// RanchWind, HerdArt; MIT © jirathip-dev). One full-screen page per fleet (swipe ←→, Others last), the
// ranch strip on top, the pasture below in bands; queued issues wait at the gate above them. Data: `/herd.json`, then the
// `/live` stream's `herd` event (polled by refresh() while it is down). Host load is the weather.
// One requestAnimationFrame loop on one clock drives every motion, current page only, paused when hidden.
const NS = 'http://www.w3.org/2000/svg', M64 = (1n << 64n) - 1n;
// RanchPainter.noise: indexed, never random per frame (Swift's wrapping UInt64 math, in BigInt).
const noise = (i, s = 0) => { const n = (BigInt(i) * 1664525n + BigInt(s) * 1013904223n) & M64; return Number(((n ^ (n >> 13n)) * 1274126177n) % 65536n) / 65535; };
const hx = s => String(s ?? '').replace(/[&<>"]/g, c => '&#' + c.charCodeAt(0) + ';');
const f1 = n => +n.toFixed(1);
// RanchWind: one gust phase (period 5.2 s), per-anchor offsets, a coherent grass wave.
const WIND = { period: 5.2, canopy: 2.8, grass: 2.1 };
const phase = t => t * 2 * Math.PI / WIND.period;
const anchor = x => ((x % 390) / 390) * 2 * Math.PI * 0.7;
const sway = (t, a, amp) => amp * (0.74 * Math.sin(phase(t) + a) + 0.26 * Math.sin(1.93 * phase(t) + a * 1.7));
const bend = (t, x, amp) => amp * (0.8 * Math.sin(phase(t) - x * 0.028) + 0.2 * Math.sin(1.9 * phase(t) - x * 0.011));
const ell = (x, y, rx, ry, fill, op = 1) => `<ellipse cx="${f1(x)}" cy="${f1(y)}" rx="${f1(rx)}" ry="${f1(ry)}" fill="${fill}"${op < 1 ? ` opacity="${op}"` : ''}/>`;
const rect = (x, y, w, h, fill, op = 1) => `<rect x="${f1(x)}" y="${f1(y)}" width="${f1(w)}" height="${f1(h)}" fill="${fill}"${op < 1 ? ` opacity="${op}"` : ''}/>`;

// The scene (390 × 320 of Corral's 390 × 640 world: sky to the rear fence and its grass). The sky is
// the page's CSS gradient (storm darkens it); clouds are their own blurred layer that only translates.
// #71: every fill is a custom property (herd.css: Corral's color(day, dark) pairs), so `#herd.night` repaints it.
function clouds() {
  let s = '';
  for (const copy of [0, 780]) for (let i = 0; i < 6; i++) {
    const x = i * 130 + 45 + copy, y = 36 + noise(i, 30) * 86;
    for (let j = 0; j < 5; j++) s += ell(x + j * 12, y - noise(j, i) * 9, 18, 5 + noise(j, 2) * 5, 'var(--cl)');
  }
  return `<svg viewBox="0 0 1560 320" aria-hidden=true>${s}</svg>`;
}
// #71 night sky (RanchPainter.sky, night): the Milky Way and the seeded stars ride one slow drift (RanchSky.celestialSpeed,
// `.cel`, a 390 tile with its neighbours: a star keeps its identity, only moves), under the hills; the moon stays put.
function night() {
  let s = '';
  for (const k of [-390, 0, 390]) {
    for (let b = 0; b < 6; b++) s += `<path d="M${k - 60} 220Q${k + 60} 74 ${k + 220} 3Q${k + 275} -20 ${k + 335} -32" stroke-width=${66 - b * 8}/>`;
  }
  s = `<g class=mw filter="url(#hmw)">${s}</g>`;
  for (let i = 0; i < 130; i++) for (const k of [-390, 0, 390]) {
    const r = 0.35 + noise(i, 7) * 0.7;
    s += ell(k + i * 3 + noise(i, 8) * 3, noise(i, 3) * 225, r, r, '#e7e5f4', f1((0.3 + 0.45 * noise(i, 9) + 0.08 * Math.sin(i)) * 100) / 100);
  }
  return `<g class=sky-night><g class=cel>${s}</g>${ell(306, 56, 16, 16, '#d3dfeb', 0.84)}${ell(301, 51, 15, 15, '#1d2841')}</g>`;
}
function scene() {
  let s = night();
  ['var(--h0)', 'var(--h1)', 'var(--h2)', 'var(--h3)'].forEach((fill, r) => {
    let d = 'M-85 330';
    for (let i = -4; i <= 14; i++) d += `L${i * 35} ${f1(147 + r * 30 + noise(i, r + 17) * 36)}`;
    s += `<path d="${d}L475 330Z" fill="${fill}"/>`;
  });
  s += '<path class=gd d="M-60 258Q165 228 450 268L450 330L-60 330Z"/>';
  for (let i = -1; i <= 2; i++) {
    const x = i * 160;
    s += `<path d="M${x + 130} 258Q${x + 40} 430 ${x + 180} 640L${x + 226} 640Q${x + 62} 438 ${x + 145} 258Z" fill="var(--lw)" opacity=".18"/>`;
    s += `<path d="M${x} 277Q${x - 35} 395 ${x + 130} 520L${x + 55} 566Q${x - 55} 389 ${x - 30} 290Z" fill="var(--sh)" opacity=".12"/>`;
  }
  for (let i = -152; i <= 1650; i++) {
    const y = 270 + noise(i, 19) * 370;
    if (y < 330) s += ell(Math.floor(i / 19) * 5 + noise(i, 8) * 5, y, 0.3 + noise(i, 2) * 0.7, 0.3, 'var(--st)', 0.13);
  }
  // Barn, door, trim and lit window; then the trees (tile ±1 too, where it reaches the frame).
  s += '<path d="M153 243L180 219L210 242L210 279L153 279Z" fill="var(--bn)"/><path d="M148 245L180 215L216 243L209 246L180 224L155 248Z" fill="var(--rf)"/>';
  s += rect(175, 252, 14, 27, 'var(--dr)') + rect(175, 235, 9, 9, 'var(--wn)', 0.8);
  for (const tile of [-1, 0, 1]) for (const [tx, ty, sc] of [[7, 225, 1], [365, 228, 1.1], [238, 258, 0.5], [326, 262, 0.38]]) {
    const x = tile * 390 + tx;
    if (x < -60 || x > 450) continue;
    // The trunk stays put; leaf clusters sway in 4 groups on the shared phase, upper ones further.
    s += rect(x - 2 * sc, ty - 8 * sc, 4 * sc, 45 * sc, 'var(--tk)');
    const g = ['', '', '', ''], lift = [0, 0, 0, 0];
    for (let i = 0; i < 22; i++) {
      const dx = (noise(i, 20) - 0.5) * 48 * sc, dy = (noise(i, 7) - 0.5) * 35 * sc;
      g[i % 4] += ell(x + dx, ty + dy, 9 * sc, 11 * sc, i % 3 === 0 ? 'var(--l1)' : 'var(--l2)', 0.9) + ell(x + dx - 2 * sc, ty + dy - 3 * sc, 5 * sc, 4 * sc, 'var(--lh)', 0.1);
      lift[i % 4] += (1.1 - (dy / (35 * sc)) * 0.35) / 6;
    }
    g.forEach((c, k) => { s += `<g class=cn data-a="${f1(anchor(x) + k * 0.5 + 0.35)}" data-l="${f1(sc * lift[k] * 100) / 100}">${c}</g>`; });
  }
  for (let i = -2; i <= 9; i++) s += rect(i * 52, 249, 6, 44, 'var(--fp)') + rect(i * 52 + 1, 249, 1.4, 44, 'var(--fh)', 0.5);
  for (const dy of [4, 22]) s += rect(-60, 256 + dy, 510, 5, 'var(--fr)') + rect(-60, 256 + dy, 510, 1.3, 'var(--fl)', 0.8);
  for (let i = -5; i <= 48; i++) {
    const x = i * 9, y = 298 + noise(i, 43) * 24;
    let b = '';
    for (let k = 0; k < 3; k++) b += `M${x + 2 * k} ${f1(y)}Q${x + 2 * k - 1} ${f1(y - 5)} ${x + 2 * k - 3} ${f1(y - 8 - noise(i, 4) * 4)}`;
    s += `<path class=gt data-x=${x} data-y="${f1(y)}" d="${b}"/>`;
  }
  // Sun / moon direction: the cast light starts at the sun's side by day, the moon's (mirrored, x 390) at night.
  s += '<rect class=wa width="390" height="320"/>';
  return `<svg class=rs viewBox="0 0 390 320" preserveAspectRatio="xMidYMax slice" aria-hidden=true>${s}</svg>`;
}

// Corral's horses (#30's port, horses.svg next to this script) as <use> symbols, shared with the Lanes
// avatars: legs rotate by the inherited `--g` (gait), manes skew by `--mane` (storm), so instances need no per-leaf DOM.
const defs = document.createElementNS(NS, 'svg');
defs.setAttribute('class', 'hdefs');
defs.innerHTML = '<defs><linearGradient id=hgr gradientUnits=userSpaceOnUse x1=0 y1=250 x2=0 y2=640><stop offset=0 stop-color="#bcc17b"/><stop offset=".5" stop-color="#8e9c58"/><stop offset=1 stop-color="#485e3b"/></linearGradient>' +
  '<linearGradient id=hgrn gradientUnits=userSpaceOnUse x1=0 y1=250 x2=0 y2=640><stop offset=0 stop-color="#63746a"/><stop offset=".5" stop-color="#415d55"/><stop offset=1 stop-color="#273f3e"/></linearGradient>' +
  '<linearGradient id=hwd gradientUnits=userSpaceOnUse x1=0 y1=0 x2=195 y2=500><stop offset=0 stop-color="#ffeeab" stop-opacity=".03"/><stop offset=1 stop-color="#ffeeab" stop-opacity="0"/></linearGradient>' +
  '<linearGradient id=hwn gradientUnits=userSpaceOnUse x1=390 y1=0 x2=195 y2=500><stop offset=0 stop-color="#bbcfe4" stop-opacity=".03"/><stop offset=1 stop-color="#bbcfe4" stop-opacity="0"/></linearGradient>' +
  '<filter id=hmw x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation=6 /></filter></defs>';
fetch(document.currentScript.src.replace(/herd\.js$/, 'horses.svg')).then(r => r.text()).then(t => {
const src = document.createElementNS(NS, 'svg');
src.innerHTML = t;
src.querySelectorAll('g.horse').forEach((g, k) => {
  const s = document.createElementNS(NS, 'symbol');
  s.id = 'hp' + (k + 1);
  s.setAttribute('viewBox', '0 0 132 100');
  s.append(g.cloneNode(true));
  s.querySelectorAll('.leg').forEach(l => {
    const a = l.style.getPropertyValue('--a') || '0deg', sign = /leg-(nf|fh)/.test(l.getAttribute('class')) ? 1 : -1;
    l.setAttribute('style', `transform-box:fill-box;transform-origin:50% 0;transform:rotate(calc(${a} + ${sign} * var(--g, 0deg)))`);
  });
  s.querySelectorAll('[fill="var(--m)"],[stroke="var(--m)"]').forEach(m => m.setAttribute('style', 'transform-box:fill-box;transform-origin:100% 100%;transform:skewX(var(--mane, 0deg))'));
  defs.append(s);
});
B.classList.add('hz');
}).catch(() => {});

B.insertAdjacentHTML('beforeend', `<div id=herd hidden><div class=hpages></div><div class=hrain></div><div class=hflash></div>
<button type=button class=hlt></button><button type=button class=hx aria-label="close herd mode">×</button><p class=hcap></p><div class=hq hidden></div></div>`);
const H = $('#herd'), PG = H.querySelector('.hpages'), cap = $('.hcap'), info = $('.hq'), rain = $('.hrain'), flash = $('.hflash');
B.prepend(defs);
// #58: the header's Herd button is our horse (the default coat), not an emoji.
$('#herd-on').innerHTML = '<svg viewBox="0 0 132 100" style="--c:#c89a56;--d:#a37a3c;--m:#e8dcc0;--b:#efe7dc" aria-hidden=true><use href="#hp2"/></svg>';
const pages = new Map();
const POSE = { working: 1, idle: 2, unknown: 2, done: 3, blocked: 4, dead: 4 };
const MARK = { working: '○', idle: '◦', unknown: '?', done: '✓', blocked: '!', dead: '✗' };
const BANDS = [['working', ['working']], ['idle', ['idle', 'unknown']], ['done', ['done']], ['blocked/dead', ['blocked', 'dead']]];
let data = null, wind = 0, storm = false, saved = get('herdp'), placed = false;

// #65: Corral's label card, 2 lines: the name, then state mark + word + age (the issue title is the tooltip).
function horse(l, k) {
  return `<button type=button class="hh ${hx(l.state)}" data-agent="${hx(l.name)}" data-k=${k}${l.title ? ` title="${hx(l.title)}"` : ''}><svg viewBox="0 0 132 100" style="${hx(l.coat)}"><use href="#hp${POSE[l.state] || 2}"/></svg>` +
    `<span class=hl><b>${hx(l.name)}</b><span><i class=${hx(l.tone)}>${MARK[l.state] || '?'} ${hx(l.shown)}</i>${l.age ? ' · ' + hx(l.age) : ''}</span></span></button>`;
}
const queued = q => `<button type=button class=hqh data-i=${q.issue}><svg viewBox="0 0 132 100" style="${hx(q.coat)}"><use href="#hp2"/></svg>` +
  `<span>#${q.issue}${q.blocked_by ? `<br>⛔#${q.blocked_by}` : ''}</span></button>`;
const tpl = h => { const t = document.createElement('template'); t.innerHTML = h; return t.content.firstElementChild; };
const attrs = (a, b) => {
  for (const { name } of [...a.attributes]) if (!b.hasAttribute(name)) a.removeAttribute(name);
  for (const { name, value } of b.attributes) if (a.getAttribute(name) !== value) a.setAttribute(name, value);
};
// #65: the horse keyed `key` (taken from `have`), patched in place to `h`: button and coat attributes, pose, label.
function keyed(have, key, h) {
  let n = have.get(key);
  have.delete(key);
  if (!n) n = tpl(h);
  else if (n._h !== h) {
    const x = tpl(h);
    attrs(n, x);
    attrs(n.firstChild, x.firstChild); // the coat (also drops a stopped trot's transform)
    n.firstChild.firstChild.setAttribute('href', x.firstChild.firstChild.getAttribute('href'));
    n.lastChild.innerHTML = x.lastChild.innerHTML;
  }
  n._h = h;
  return n;
}
// #65: an update patches the page in place, so its scroll never moves: horses are keyed by agent (queued ones by
// issue), re-parented between bands, added or removed one by one; a band only comes or goes with its first or last horse.
function patch(hb, lanes, queue) {
  const have = new Map([...hb.querySelectorAll('.hh')].map(n => [n.dataset.agent, n]));
  const haveq = new Map([...hb.querySelectorAll('.hqh')].map(n => [n.dataset.i, n]));
  const groups = [['queue', 'queued', queue, haveq, q => String(q.issue), queued, 'gate'],
    ...BANDS.map(([name, states]) => [name.split('/')[0], name, lanes.filter(l => states.includes(l.state)), have, l => l.name, horse, 'field'])];
  // Gone horses leave first, so a kept one never steps around them.
  const want = new Set(groups.flatMap(g => g[2].map(g[4])));
  for (const m of [have, haveq]) for (const [k, n] of m) if (!want.has(k)) { n.remove(); m.delete(k); }
  let at = null;
  const drop = [];
  for (const [cls, name, items, map, id, make, box] of groups) {
    let b = hb.querySelector(':scope > .b-' + cls);
    if (!items.length) { if (b) drop.push(b); continue; }
    if (!b) {
      b = tpl(`<div class="band b-${cls}"><h4>${name} <span></span></h4><div class=${box}></div></div>`);
      if (at) at.after(b); else hb.prepend(b);
    }
    const c = b.querySelector('h4 span');
    if (c.textContent !== String(items.length)) c.textContent = items.length;
    const f = b.lastChild;
    items.forEach((it, i) => { const n = keyed(map, id(it), make(it, i)); if (f.children[i] !== n) f.insertBefore(n, f.children[i] || null); });
    at = b;
  }
  drop.forEach(b => b.remove());
  const e = hb.querySelector(':scope > .empty');
  if (at) { if (e) e.remove(); } else if (!e) hb.append(tpl('<p class=empty>no horses here</p>'));
}

function page(key) {
  const p = document.createElement('section');
  p.className = 'hp';
  p.innerHTML = `<div class=hs><p><b></b> <span class=n></span> <span class=dots></span></p><p class=sum></p></div><div class=hn></div>
<div class=hr><div class=hc>${clouds()}</div>${scene()}<div class=fog></div></div><div class=hb></div>`;
  p._trees = [...p.querySelectorAll('.cn')].map(g => [g, +g.dataset.a, +g.dataset.l]);
  p._cel = p.querySelector('.cel');
  p._grass = [...p.querySelectorAll('.gt')].map(g => [g, +g.dataset.x, +g.dataset.y]);
  p._key = key;
  return p;
}

function draw() {
  if (!data) return;
  const list = data.fleets.map(f => [f.name, f.lanes, f.queue]);
  if (data.others.length) list.push(['Others', data.others, []]);
  for (const [k, p] of pages) if (!list.some(x => x[0] === k)) { p.remove(); pages.delete(k); }
  list.forEach(([key, lanes, queue], n) => {
    const p = pages.get(key) || page(key);
    pages.set(key, p);
    // Never re-append a placed page: a detached scroller forgets its scrollTop (#65, the snap to the top).
    if (PG.children[n] !== p) PG.insertBefore(p, PG.children[n] || null);
    p.querySelector('.hs b').textContent = key;
    p.querySelector('.hs .n').textContent = `· ${lanes.length} here`;
    p.querySelector('.hs .dots').textContent = list.map((_, i) => i === n ? '●' : '○').join('');
    const count = states => lanes.filter(l => states.includes(l.state)).length;
    // #65 A: queued first, zero counts left out, a bare `·` (a full mono cell) between, so the line fits the 28 px strip at 390 px without an ellipsis.
    p.querySelector('.sum').textContent = [[queue.length, 'queued'], [count(['blocked', 'dead']), 'blocked'], [count(['working']), 'working'],
      [count(['idle', 'unknown']), 'idle'], [count(['done']), 'done']].filter(c => c[0]).map(c => c.join(' ')).join('·');
    // Queued horses get their own row above the working band; a band with no horses is not drawn.
    const top = p.scrollTop;
    patch(p.querySelector('.hb'), lanes, queue);
    if (p.scrollTop !== top) p.scrollTo({ top, behavior: 'instant' });
    p._queue = queue;
    p._walk = [...p.querySelectorAll('.hh.working svg')];
  });
  needs();
  weather(data.host);
  if (!placed && list.length) {
    placed = true;
    const i = Math.max(0, list.findIndex(x => x[0] === saved));
    PG.scrollLeft = i * PG.clientWidth;
  }
  place();
  go();
}
function herdData(j) { if (!DBG.get('weather')) { data = j; draw(); } }
async function herdPull() {
  if (H.hidden) return;
  try {
    const w = DBG.get('weather'), r = await fetch(R + 'herd.json' + (w ? '?weather=' + encodeURIComponent(w) : ''), { cache: 'no-store' });
    if (r.ok) { data = await r.json(); draw(); }
  } catch (e) {}
}

// #40/#42: each page carries its fleet's needs-you cards (the same per-audience HTML as the tabs).
function needs() {
  const keys = [...pages.keys()];
  pages.forEach((p, k) => {
    const mine = [...$$('#needs > .ask')].filter(c => {
      const f = (c.querySelector('.m') || {}).textContent || '';
      return k === 'Others' ? !keys.includes(f.split(' · ')[0]) : f.split(' · ')[0] === k;
    });
    p.querySelector('.hn').replaceChildren(...mine.map(c => c.cloneNode(true)));
  });
}
new MutationObserver(needs).observe($('#needs'), { childList: true, subtree: true });

// Wind = load/cpu ÷ limit (0..1): canopy/grass ×1..3, clouds as fast. The spawn gate refusing = storm;
// memory pressure = fog. Reduced motion keeps the tint only.
function weather(h) {
  wind = Math.min(1, Math.max(0, (h.load || 0) / (h.limit || 1)));
  storm = !!h.gate;
  H.classList.toggle('storm', storm);
  H.classList.toggle('fog1', h.pressure === 'warn');
  H.classList.toggle('fog2', h.pressure === 'critical');
  H.classList.toggle('gust', wind >= 0.5);
  if (!storm || still.matches) H.style.setProperty('--mane', '0deg');
  cap.textContent = `load ${h.load == null ? '?' : h.load.toFixed(2)} · mem ${h.pressure || '?'} · swap ${h.swapouts == null ? '?' : Math.round(h.swapouts)}`;
}

// The scene's frame inside its box (xMidYMax slice): scale and left edge for the cloud layer.
function place() {
  for (const p of pages.values()) {
    const r = p.querySelector('.hr'), w = r.clientWidth, h = r.clientHeight, k = Math.max(w / 390, h / 320);
    p._k = k;
    const c = p.querySelector('.hc');
    c.style.left = (w - 390 * k) / 2 + 'px';
    c.style.top = h - 320 * k + 'px';
    c.style.width = 1560 * k + 'px';
  }
}
addEventListener('resize', place);
const here = () => [...pages.values()][Math.round(PG.scrollLeft / Math.max(1, PG.clientWidth))];
function herdPage() { return H.hidden ? null : here(); }
PG.addEventListener('scroll', () => { const p = here(); if (p && p._key !== saved) { saved = p._key; put('herdp', saved); } }, { passive: true });

let raf = 0, lastT = 0, drift = 0, cel = 0, flashAt = -99, nextFlash = 0, frames = 0, fpsAt = 0;
function frame(ms) {
  raf = 0;
  if (H.hidden || document.hidden || still.matches) return;
  const t = ms / 1000, dt = Math.min(0.1, t - (lastT || t)), m = 1 + 2 * wind, p = here();
  lastT = t;
  drift = (drift + dt * 2.4 * m) % 780;
  cel = (cel + dt * 0.7) % 390;
  if (p) {
    for (const [g, a, l] of p._trees) g.setAttribute('transform', `translate(${f1(sway(t, a, WIND.canopy * l * m))} ${f1(sway(t, a + 1.9, 0.5 * l * m))})`);
    for (const [g, x, y] of p._grass) { const b = bend(t, x, WIND.grass * m) / 10; g.setAttribute('transform', `matrix(1 0 ${-b.toFixed(3)} 1 ${(b * y).toFixed(2)} 0)`); }
    if (H.classList.contains('night')) p._cel.setAttribute('transform', `translate(${cel.toFixed(2)} 0)`);
    p.querySelector('.hc').style.transform = `translateX(${((drift - 780) * (p._k || 1)).toFixed(1)}px)`;
    // Working horses trot (diagonal leg pairs, Corral's 0.4 s stride) and roam a little.
    p._walk.forEach((s, i) => {
      const g = Math.sin(t * Math.PI / 0.4 + i * 1.3);
      s.style.setProperty('--g', (g * 16).toFixed(1) + 'deg');
      s.style.transform = `translate(${(Math.sin(t * 0.5 + i * 2.1) * 10).toFixed(1)}px, ${(-Math.abs(g) * 1.5).toFixed(1)}px)`;
    });
  }
  if (storm) {
    H.style.setProperty('--mane', (-12 - 6 * Math.sin(t * 3.1)).toFixed(1) + 'deg');
    rain.style.transform = `translate(${(-(t * 90) % 60).toFixed(1)}px, ${((t * 520) % 60).toFixed(1)}px)`;
    // Lightning: at most once every 20 s.
    if (t >= nextFlash) { flashAt = t; nextFlash = t + 20 + Math.random() * 25; }
    flash.style.opacity = Math.max(0, 0.7 - (t - flashAt) * 3.5).toFixed(2);
  }
  frames++;
  if (DBG.has('fps') && t - fpsAt >= 1) { cap.dataset.fps = Math.round(frames / (t - fpsAt)) + ' fps'; frames = 0; fpsAt = t; }
  raf = requestAnimationFrame(frame);
}
function go() { if (!raf && !H.hidden && !document.hidden && !still.matches) { lastT = 0; nextFlash = performance.now() / 1000 + 4; raf = requestAnimationFrame(frame); } }
document.addEventListener('visibilitychange', go);

// Taps: a horse opens the Lanes sheet (#43: card, transcript, Stop/Kill/Clean, reply); a queued one its row.
H.addEventListener('click', e => {
  const h = e.target.closest('.hh'), q = e.target.closest('.hqh');
  if (h) {
    const l = [...$$('.lane')].find(x => x.dataset.agent === h.dataset.agent);
    if (l) open(l);
  } else if (q) {
    const it = (here()._queue || []).find(x => String(x.issue) === q.dataset.i);
    if (!it) return;
    info.innerHTML = `<p><b>#${it.issue}</b> queued</p>${it.title ? `<p>${hx(it.title)}</p>` : ''}${it.blocked_by ? `<p class=r>⛔ blocked by #${it.blocked_by}</p>` : ''}<p class=m>tap to close</p>`;
    info.hidden = false;
  } else if (e.target.closest('.hq')) info.hidden = true;
});
// The 🐎 button enters Herd mode, ✕ leaves; remembered, so the app reopens where it was.
function herd(on) {
  H.hidden = !on;
  document.documentElement.classList.toggle('herding', on);
  put('herd', on ? '1' : '');
  if (on) { herdPull(); go(); }
}
$('#herd-on').onclick = () => herd(true);
$('.hx').onclick = () => herd(false);
// #71 ranch light, Corral's Auto/Day/Night: [A]/[☀]/[☾] left of [×] cycles them, remembered on this phone (`?light=` overrides
// for a demo). Auto is the phone clock, Day 07:00–19:00, re-checked every minute and on resume.
const LIGHTS = ['auto', 'day', 'night'], ICON = { auto: 'A', day: '☀', night: '☾' }, lb = $('.hlt');
const nightAt = (l, d) => l === 'night' || (l === 'auto' && (d.getHours() < 7 || d.getHours() >= 19));
let light = [DBG.get('light'), get('herd-light')].find(l => LIGHTS.includes(l)) || 'auto';
function ranch() {
  const n = nightAt(light, new Date());
  H.classList.toggle('night', n);
  lb.textContent = ICON[light];
  lb.setAttribute('aria-label', `ranch light: ${light}${light === 'auto' ? ` (${n ? 'night' : 'day'} now)` : ''}`);
}
lb.onclick = () => { light = LIGHTS[(LIGHTS.indexOf(light) + 1) % 3]; put('herd-light', light); ranch(); };
ranch();
setInterval(ranch, 60000);
document.addEventListener('visibilitychange', ranch);
// #39 demo: a weather switcher sets the host values client-side, as `/herd.json?weather=` does on loopback.
const WX = { calm: [0.1, 'normal'], gusty: [0.7, 'normal'], storm: [1.3, 'normal'], fog: [0.3, 'warn'] };
if (DEMO) H.insertAdjacentHTML('beforeend', `<p class=hwx>${Object.keys(WX).map(w => `<button type=button value=${w}>${w}</button>`).join('')}</p>`);
H.addEventListener('click', e => {
  const w = e.target.closest('.hwx button'), h = data && data.host;
  if (!w || !h) return;
  const [k, p] = WX[w.value], why = [p !== 'normal' && 'memory pressure ' + p, k > 1 && `load/cpu ${(k * h.limit).toFixed(2)} > ${h.limit}`].filter(Boolean);
  weather(Object.assign(h, { load: k * h.limit, pressure: p, swapouts: 0, gate: why.join('; ') || null }));
  $$('.hwx button').forEach(b => b.classList.toggle('on', b === w));
});
herd(get('herd') === '1' || DBG.get('herd') === '1');

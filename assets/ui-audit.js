// #63 gate 2 (docs/ui.md), shipped only in the `serve --export --demo` page: `?audit=<view>` sets up one view
// for scripts/ui-audit.sh; `&check=1` then measures it at 390 px and writes {"view","controls","outliers"} into
// <pre id=audit>. Rows (summary, lane rows, horses) are 44 px tap rows, not 28 px controls.
(() => {
  const V = DBG.get('audit'); // serve.js read the query before it tidied the URL
  if (!V) return;
  const hp = n => H.querySelectorAll('.hp')[n];
  // #68: each inline control's failure path with a canned long reply (the old 401 pair text, a 140-char refusal,
  // a send error); measured after each step (`mid`), then with the rest at the end.
  const F = window.fetch, early = [], wait = () => new Promise(r => setTimeout(r, 300));
  const LONG = {
    pair: 'pair this device: tap Pair this phone, then `herdsman serve approve <code>` on the Mac',
    sub: 'push subscribe refused: the endpoint https://web.push.apple.com/QGx2c3Rlc3Qtc3Vic2NyaXB0aW9uLWVuZHBvaW50LWlk is not https or not a known push service',
    kill: 'REFUSED UNSAFE_CLEAN: acme-web-impl-48-contrast-audit-for-every-settings-pane-abxx has 3 unpushed commits and a dirty tree; push them first',
    dry: 'dry run; would push 3 commits and 2 dirty files to wip/acme-web-impl-48-contrast-audit-for-every-settings-pane-abxx-20261031-1943, then close pane w1:p5',
    send: 'error: herdr pane send w1:p5 failed: pane closed while sending (exit 1); the message was not delivered, nothing was typed into the agent',
  };
  const canned = async (status, text, then) => { window.fetch = (u, o) => o && o.method === 'POST' ? Promise.resolve(new Response(text, { status })) : F(u, o); then(); await wait(); };
  const mid = what => early.push(...measure().outliers.map(o => `${what}: ${o}`));
  const VIEWS = {
    queue: () => tab('queue'),
    'queue-open': () => { tab('queue'); const d = $('#queue details.q'); d.open = true; d.scrollIntoView(); },
    // #139: the demo's acme-web #46 row, its ask decided by default, re-answered through [change] (3 long choices),
    // opened and moved to the top (a scrolled view shoots blank).
    'queue-change': () => {
      tab('queue');
      const d = [...$$('#queue details.q')].find(d => d.querySelector('[name=choice]'));
      if (!d) return early.push('no queue row with a [change] form in the demo');
      d.open = true;
      $('#queue').prepend(d);
      if (!DBG.get('check')) $('#needs').hidden = true; // the shot shows the row; the check keeps the needs cards
    },
    // #92: the demo's `driver = "herdsman"` fleet (docs): its heading and `driver: herdsman · last pass …` line, moved
    // to the top (a scrolled view shoots blank).
    'queue-drive': () => {
      tab('queue');
      const p = [...$$('#queue p.m')].find(p => p.textContent.startsWith('driver:'));
      $('#queue').prepend(p.previousElementSibling, p);
    },
    lanes: () => tab('lanes'),
    sheet: async () => { // the longest agent name, then kill refused, kill's dry run, clean refused, a send error
      tab('lanes');
      open([...$$('.lane')].sort((a, b) => b.dataset.agent.length - a.dataset.agent.length)[0]);
      await wait();
      await canned(400, LONG.kill, () => run('kill'));
      mid('kill refused');
      await canned(200, LONG.dry, () => run('kill'));
      mid('kill dry run');
      $('.cf').hidden = true;
      say('', true);
      await canned(400, LONG.kill, cleanable);
      mid('clean refused');
      await canned(502, LONG.send, () => $('.reply').requestSubmit());
    },
    // #83: the demo's parked lane (no pane, its work on PR #15): the card's parked line, [clean] disabled with its
    // reason in p.st, [stop]/[kill] hidden, and nothing in the sheet in the red (danger) colour.
    'sheet-parked': async () => {
      tab('lanes');
      const row = $('.lane[data-agent="docs-impl-7"]');
      if (!row) return early.push('no parked lane docs-impl-7 in the demo');
      row.scrollIntoView({ block: 'start', behavior: 'instant' });
      open(row);
      await wait();
      await canned(409, 'REFUSED UNSAFE_CLEAN: docs-impl-7: only after merge (PR #15 open)', cleanable);
      const probe = S.appendChild(Object.assign(document.createElement('i'), { className: 'r' })), red = getComputedStyle(probe).color;
      probe.remove();
      if (st.textContent !== 'clean: only after merge (PR #15 open)') early.push(`p.st reads "${st.textContent}", not clean's reason`);
      if (!$('.top').textContent.includes('parked · work on PR #15')) early.push('the card has no `parked · work on PR #15` line');
      for (const op of ['stop', 'kill']) if (seen($(`[data-op=${op}]`))) early.push(`[${op}] shown on a lane with no pane`);
      if (!$('[data-op=clean]').disabled) early.push('[clean] enabled before merge');
      if ($('.now').textContent !== 'no pane: the transcript closed with it') early.push(`chat reads "${$('.now').textContent.slice(0, 40)}"`);
      for (const e of [S, ...S.querySelectorAll('*')].filter(seen)) if (e.matches('.r') || getComputedStyle(e).color === red) early.push(`red in the sheet: ${e.outerHTML.slice(0, 60)}`);
    },
    // #95: the demo's Others agent (not a lane): only [close pane], the reason in p.st, nothing in the red colour.
    'sheet-other': async () => {
      tab('lanes');
      const row = $('.lane[data-role=other]');
      if (!row) return early.push('no Others agent in the demo');
      open(row);
      await wait();
      const probe = S.appendChild(Object.assign(document.createElement('i'), { className: 'r' })), red = getComputedStyle(probe).color;
      probe.remove();
      if (st.textContent !== 'not a herdsman lane: herdsman can only close its pane') early.push(`p.st reads "${st.textContent}"`);
      for (const op of ['stop', 'kill', 'clean']) if (seen($(`[data-op=${op}]`))) early.push(`[${op}] shown on an Others agent`);
      if (!seen($('[data-op=close]'))) early.push('no [close pane] on an Others agent');
      for (const e of [S, ...S.querySelectorAll('*')].filter(seen)) if (e.matches('.r') || getComputedStyle(e).color === red) early.push(`red in the sheet: ${e.outerHTML.slice(0, 60)}`);
    },
    // #70: the handle is ≥ 56 px; a −200 px pointer drag moves the sheet mid-drag, release snaps to a detent,
    // and close leaves it shown, sliding off-screen, for a frame before `hidden` (the slide exists). Virtual time
    // (--virtual-time-budget) runs no frames and freezes transitions: rAF is a 16 ms timer here, and the slide is
    // read as its target transform while `.out` is on.
    'sheet-drag': async () => {
      window.requestAnimationFrame = f => setTimeout(() => f(performance.now()), 16);
      tab('lanes');
      open($('.lane'));
      await wait();
      const r = hd.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2, frame = () => new Promise(f => requestAnimationFrame(() => requestAnimationFrame(f)));
      const pe = (type, y) => hd.dispatchEvent(new PointerEvent(type, { pointerId: 1, pointerType: 'touch', isPrimary: true, clientX: x, clientY: y, bubbles: true }));
      const tz = () => new DOMMatrix(getComputedStyle(sh).transform).m42, aim = () => new DOMMatrix(sh.style.transform).m42, ds = ['full', 'mid', 'half'].map(off), t0 = aim();
      if (hd.offsetHeight < 56) dragged.push(`handle ${hd.offsetHeight}px tall, under 56`);
      if (Math.abs(t0 - ds[1]) > 1) dragged.push(`opened at ${t0 | 0}px, not the 78 % detent ${ds[1] | 0}`);
      pe('pointerdown', y);
      for (let i = 1; i <= 4; i++) {
        pe('pointermove', y - 50 * i);
        await frame();
        if (i === 2 && Math.abs(tz() - (t0 - 100)) > 1) dragged.push(`mid-drag (−100 px) the sheet is at ${tz() | 0}px, not ${t0 - 100 | 0}`);
      }
      pe('pointerup', y - 200);
      await wait();
      if (!ds.some(o => Math.abs(o - aim()) < 1)) dragged.push(`released at ${aim() | 0}px, not a detent (${ds.map(o => o | 0)})`);
      close();
      await frame();
      const slide = getComputedStyle(sh).transitionDuration;
      if (S.hidden || !S.matches('.out') || aim() < sh.offsetHeight - 1 || slide !== '0.22s') dragged.push(`a frame after close: hidden ${S.hidden}, sliding to ${aim() | 0}px of ${sh.offsetHeight} over ${slide}: no slide`);
      await wait();
      if (!S.hidden) dragged.push('300 ms after close the sheet is still shown');
      if (!DBG.get('check')) { open($('.lane')); S.classList.add('drag'); snap('half'); } // the screenshot: the sheet at half, unanimated
    },
    host: async () => { // the live Host tab's Alerts (the export leaves it out) as the Home Screen app shows it: an error, then unpaired
      tab('host');
      const push = Object.assign(document.createElement('button'), { id: 'push', type: 'button', textContent: 'Enable alerts' });
      push.dataset.key = 'AA';
      const row = Object.assign(document.createElement('p'), { className: 'kv', textContent: 'on · 1 device · last sent 3m ago' }); // #87
      $('#host').append(Object.assign(document.createElement('h2'), { textContent: 'Alerts' }), row, push);
      tidy($('#host'));
      Object.defineProperty(window, 'Notification', { value: { requestPermission: async () => 'granted' } });
      const sub = { toJSON: () => ({ endpoint: 'e', keys: { p256dh: 'p', auth: 'a' } }) };
      Object.defineProperty(navigator, 'serviceWorker', { value: { ready: Promise.resolve({ pushManager: { subscribe: async () => sub } }) } });
      const st = () => ($('#push + p.st') || {}).textContent;
      await canned(400, LONG.sub, () => $('#push').click());
      if (st() !== LONG.sub) early.push(`alerts error: p.st reads "${st()}", not the reply`);
      mid('alerts error');
      await canned(401, LONG.pair, () => $('#push').click());
      if (!$('#push').disabled || st() !== 'pair this phone first') early.push(`unpaired: [enable alerts] disabled ${$('#push').disabled}, p.st "${st()}"`);
      $('#push').hidden = false;
      // `--screenshot` captures the document's top: the shot drops the needs cards so Alerts is in it (the check keeps them).
      if (!DBG.get('check')) { $('#needs').hidden = true; scrollTo(0, 0); }
    },
    events: () => tab('events'),
    herd1: () => { herd(true); light = 'day'; ranch(); }, // #71: day shots, whatever the runner's clock says
    herd2: () => { herd(true); light = 'day'; ranch(); },
    // #71: Auto is night before 07:00 and from 19:00; [A] cycles auto → day → night, each stored; a fresh load of the
    // page on this phone (an iframe: virtual time runs no reload) comes back at the last tap, night: the night shot.
    'herd-night': async () => {
      herd(true);
      const at = (h, m) => new Date(2026, 10, 1, h, m);
      for (const [h, m, n] of [[6, 59, true], [7, 0, false], [18, 59, false], [19, 0, true]]) if (nightAt('auto', at(h, m)) !== n) early.push(`auto at ${h}:${m}: not ${n ? 'night' : 'day'}`);
      if (light !== 'auto') early.push(`a new phone starts at ${light}, not auto`);
      for (const want of ['day', 'night', 'auto', 'day', 'night']) {
        lb.click();
        const n = nightAt(want, new Date()), label = 'ranch light: ' + want + (want === 'auto' ? ` (${n ? 'night' : 'day'} now)` : '');
        if (lb.getAttribute('aria-label') !== label || lb.textContent !== ICON[want] || get('herd-light') !== want || H.classList.contains('night') !== n) early.push(`tap to ${want}: [${lb.textContent}] "${lb.getAttribute('aria-label')}", stored ${get('herd-light')}, night ${H.classList.contains('night')}`);
      }
      const f = Object.assign(document.createElement('iframe'), { src: 'index.html', hidden: true });
      await new Promise(r => { f.onload = r; B.append(f); });
      await wait();
      const d = f.contentDocument, b = d.querySelector('.hlt');
      if (!b || b.textContent !== '☾' || !d.querySelector('#herd.night')) early.push(`after a reload: [${b && b.textContent}], #herd "${(d.querySelector('#herd') || {}).className}": the choice is lost`);
      f.remove();
    },
    'herd-night-storm': async () => { // the demo's [storm] at night: darker sky, rain on
      herd(true);
      light = 'night';
      ranch();
      await wait();
      await wait();
      $('.hwx button[value=storm]').click();
      if (!H.matches('.night.storm')) early.push(`#herd is "${H.className}", not night and storm`);
    },
    'herd-scroll': () => herd(true),
    'queue-scroll': () => { tab('queue'); $$('#queue details.q').forEach(d => { d.open = true; }); },
    toast: () => { // #66: a fixture ▲ POST (the redirect a live server sends), through the page's fetch-submit path
      tab('queue');
      const r = Object.defineProperties(new Response(''), { redirected: { value: true }, url: { value: location.origin + '/?msg=moved%20%2312%20before%20%2318' } });
      window.fetch = () => Promise.resolve(r);
      const f = [...$$('#queue form')].find(f => f.elements.op.value === 'mv');
      f.inert = false;
      f.requestSubmit();
      setTimeout(() => { toasted = $('.msg.g') && $('.msg.g').textContent; }, 500);
    },
  };
  let toasted = null;
  const dragged = [];
  const HP = { herd1: 0, herd2: 1, 'herd-night': 0, 'herd-night-storm': 0 }; // the views measuring one Herd page
  const ROOTS = { sheet: () => [$('#sheet')], 'sheet-parked': () => [$('#sheet')], 'sheet-other': () => [$('#sheet')], ...Object.fromEntries(Object.entries(HP).map(([v, n]) => [v, () => [hp(n), $('.hlt'), $('.hx'), $('.hwx')]])) };
  const CTL = 'button:not(.lr,.hh,.hqh), .pk, [role=button]', ROW = 'summary, .lr, .hh, .hqh', NAME = '.nw, .hl b';
  const CHOICE = 'button[name=choice], .pick'; // amendment A (#139: wherever it renders): wraps, ≤ 4 lines, instead of 28 px
  // A closed <details> keeps its content's boxes (content-visibility), so ask the browser what is shown.
  const seen = e => e.checkVisibility({ visibilityProperty: true });
  const label = e => (e.getAttribute('aria-label') || e.textContent).trim().replace(/\s+/g, ' ').slice(0, 24) || e.tagName.toLowerCase();
  function check() {
    let res;
    try { res = (SCROLL[V] || measure)(); } catch (e) { res = { view: V, controls: 0, outliers: ['audit error: ' + e.message] }; }
    if (innerWidth !== 390) res.outliers.push(`viewport ${innerWidth} px wide, not 390`); // #68: new-headless Chrome keeps windows ≥ 500 px
    // #114 app-like: no pinch or double-tap zoom.
    const vp = $('meta[name=viewport]').content, ta = getComputedStyle(document.documentElement).touchAction;
    if (vp !== 'width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no,viewport-fit=cover') res.outliers.push(`viewport meta "${vp}"`);
    if (ta !== 'manipulation') res.outliers.push(`html touch-action ${ta}, not manipulation`);
    const out = document.createElement('pre');
    out.id = 'audit';
    out.textContent = JSON.stringify(res);
    B.append(out);
  }
  function measure() {
    $$('[inert]').forEach(e => { e.inert = false; });
    const roots = (ROOTS[V] || (() => [B]))(), bad = [];
    const all = sel => roots.flatMap(r => [...(r.matches(sel) ? [r] : []), ...r.querySelectorAll(sel)]).filter(seen);
    const ctl = all(CTL), rows = all(ROW);
    for (const e of ctl) {
      const cs = getComputedStyle(e), lines = (e.offsetHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom)) / parseFloat(cs.lineHeight);
      if (!e.matches(CHOICE) && cs.height !== '28px') bad.push(`${label(e)}: control height ${cs.height}`);
      if (e.matches(CHOICE) && (e.offsetHeight < 28 || lines > 4.05)) bad.push(`${label(e)}: choice ${e.offsetHeight}px, ${lines.toFixed(1)} lines (28 px .. 4 lines)`);
      // #139: the CSS brackets a label once; one that carries its own reads `[[change]]`.
      if (/^\s*\[/.test(e.textContent) && getComputedStyle(e, '::before').content === '"["') bad.push(`${label(e)}: label in brackets of its own, shown [[…]]`);
    }
    for (const e of [...ctl, ...rows]) {
      e.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }); // `.hp` scrolls smooth otherwise
      const r = e.getBoundingClientRect(), cx = r.left + r.width / 2, cy = r.top + r.height / 2;
      for (const [x, y] of [[cx - 16, cy], [cx + 16, cy], [cx, cy - 16], [cx, cy + 16]]) {
        if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) continue;
        const h = document.elementFromPoint(x, y);
        if (!h || !e.contains(h)) { bad.push(`${label(e)}: tap area under 44 px, (${x | 0},${y | 0}) hits ${h ? h.outerHTML.slice(0, 40) : 'nothing'}`); break; }
      }
    }
    if (V === 'herd2') hp(1).scrollIntoView();
    for (const r of roots) {
      const w = document.createTreeWalker(r, NodeFilter.SHOW_TEXT);
      for (let t; (t = w.nextNode());) {
        const e = t.parentElement;
        if (!t.data.trim() || !seen(e) || e.closest('svg, .gl, .asc')) continue;
        const fs = getComputedStyle(e).fontSize, input = e.closest('input, textarea, select');
        if (input ? fs !== '16px' : !['11px', '13px'].includes(fs)) bad.push(`"${t.data.trim().slice(0, 20)}": font ${fs}`);
        // Text past its box's sides, unless that box ellipsises it (the button's `]` ring is not text).
        let b = e;
        while (['inline', 'contents'].includes(getComputedStyle(b).display)) b = b.parentElement;
        const g = document.createRange();
        g.selectNodeContents(t);
        const tr = g.getBoundingClientRect(), br = b.getBoundingClientRect();
        if (!input && (tr.right > br.right + 1 || tr.left < br.left - 1) && getComputedStyle(b).textOverflow !== 'ellipsis') bad.push(`"${t.data.trim().slice(0, 20)}": text overflows its box by ${Math.max(tr.right - br.right, br.left - tr.left) | 0}px`);
      }
      // #68: no box holds more than it shows unless it ellipsises (or scrolls by design); nothing past the screen's right edge.
      // Tap rings (`]`, the queued horse's ::before) are hit area, not content: off while measuring. The sky's clouds drift past its clipped sides.
      const ring = Object.assign(document.createElement('style'), { textContent: 'button::after,.pk::after,.hqh::before{display:none!important}' });
      B.append(ring);
      for (const e of [r, ...r.querySelectorAll('*')]) {
        if (e.closest('svg, .hr') || !seen(e)) continue;
        const cs = getComputedStyle(e), what = `${e.localName}${typeof e.className === 'string' && e.className ? '.' + e.className.split(' ')[0] : ''} "${label(e)}"`;
        if (cs.display === 'inline' || /auto|scroll/.test(cs.overflowX)) continue;
        if (e.scrollWidth > e.clientWidth + 1 && !(cs.textOverflow === 'ellipsis' && cs.overflowX === 'hidden')) bad.push(`${what}: ${e.scrollWidth}px of content in ${e.clientWidth}px`);
        if (e.getBoundingClientRect().right > innerWidth + 1) bad.push(`${what}: right edge ${e.getBoundingClientRect().right | 0} past the screen's ${innerWidth}`);
      }
      ring.remove();
      for (const e of [...r.querySelectorAll(NAME)].filter(seen)) {
        const cs = getComputedStyle(e), h = e.getBoundingClientRect().height;
        const lh = cs.lineHeight === 'normal' ? 1.2 * parseFloat(cs.fontSize) : parseFloat(cs.lineHeight);
        if (h > 2 * lh + 1) bad.push(`${label(e)}: name ${h | 0}px tall, over 2 lines`);
      }
    }
    // #139: nothing in an open queue row's card pokes past the card's right edge.
    for (const d of all('details.q[open]')) {
      const right = d.getBoundingClientRect().right;
      for (const e of [...d.querySelectorAll('*')].filter(seen)) if (e.getBoundingClientRect().right > right + 1) bad.push(`${label(e)}: right edge ${e.getBoundingClientRect().right | 0} past the card's ${right | 0}`);
    }
    const se = document.scrollingElement;
    if (se.scrollWidth > se.clientWidth) bad.push(`page scrolls sideways: ${se.scrollWidth}>${se.clientWidth}`);
    if (V in HP) {
      const p = hp(HP[V]), pr = p.getBoundingClientRect();
      for (const e of [p.querySelector('.hs'), $('.hlt'), $('.hx')]) if (e.offsetHeight !== 28) bad.push(`${e.className}: Herd strip/[A]/[×] ${e.offsetHeight}px, not 28`);
      // #71: the ranch-light button sits inside the strip, left of [×] (the tap checks above scrolled the page, and
      // in a storm #herd itself: the rain tile overflows it, which only scrollIntoView can scroll).
      p.scrollTo({ top: 0, behavior: 'instant' });
      H.scrollTop = 0;
      const lt = $('.hlt').getBoundingClientRect(), xr = $('.hx').getBoundingClientRect(), sr = p.querySelector('.hs').getBoundingClientRect();
      if (lt.top < sr.top || lt.bottom > sr.bottom || lt.right > xr.left) bad.push(`[${$('.hlt').textContent}] at ${lt.left | 0}..${lt.right | 0} × ${lt.top | 0}..${lt.bottom | 0}: not in the strip (${sr.top | 0}..${sr.bottom | 0}) left of [×] (${xr.left | 0})`);
      // #65: Corral's size: lane horses 132 px wide, queued ones 53 (40 %).
      for (const [sel, w] of [['.hh svg', 132], ['.hqh svg', 53]]) {
        for (const e of p.querySelectorAll(sel)) { const x = e.getBoundingClientRect().width; if (Math.abs(x - w) > 1) bad.push(`${label(e.parentNode)}: ${sel} ${x.toFixed(1)}px wide, not ${w}`); }
      }
      // #65 A: nothing on the page pokes past its sides (`.hp` clips, so the page-level check cannot see it).
      for (const e of p.querySelectorAll('.hs *, .hb *')) {
        if (e.parentElement.closest('svg') || !seen(e) || getComputedStyle(e).display === 'inline') continue; // inline text: its box clips it
        const r = e.getBoundingClientRect();
        if (r.left < pr.left - 1 || r.right > pr.right + 1) bad.push(`${label(e)}: past the page's side (${r.left | 0}..${r.right | 0} of ${pr.left | 0}..${pr.right | 0})`);
      }
      // #65 A: scrolled to the end, no text sits under the weather caption or the demo's switcher.
      p.scrollTo({ top: p.scrollHeight, behavior: 'instant' });
      const bars = [cap, $('.hwx')].filter(Boolean).map(e => [e.className, e.getBoundingClientRect()]), w = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
      for (let t; (t = w.nextNode());) {
        if (!t.data.trim() || !seen(t.parentElement)) continue;
        const g = document.createRange();
        g.selectNodeContents(t);
        const r = g.getBoundingClientRect();
        for (const [k, c] of bars) if (r.bottom > c.top && r.top < c.bottom && r.right > c.left && r.left < c.right) bad.push(`"${t.data.trim().slice(0, 20)}": under .${k}`);
      }
      // #65 A: no strip text is cut to an ellipsis, also at the owner's counts (13 queued, 11 done, 5 idle, 3 working).
      // #114: every queued horse in a row stands on one ground line (its SVG's bottom ±1 px), blocked or not, and
      // none overlaps the gate post (`.gate::after`).
      const ground = () => {
        for (const g of p.querySelectorAll('.gate')) {
          const a = getComputedStyle(g, '::after'), post = g.getBoundingClientRect().left + parseFloat(a.left) + parseFloat(a.width), rows = new Map();
          for (const h of g.querySelectorAll('.hqh')) {
            const r = h.getBoundingClientRect(), y = h.querySelector('svg').getBoundingClientRect().bottom, k = Math.round(r.top);
            if (r.left < post) bad.push(`${label(h)}: left ${r.left | 0} over the gate post (to ${post | 0})`);
            if (!rows.has(k)) rows.set(k, []);
            rows.get(k).push([label(h), y]);
          }
          for (const row of rows.values()) for (const [n, y] of row) if (Math.abs(y - row[0][1]) > 1) bad.push(`${n}: feet at ${y.toFixed(1)}, ${row[0][0]} at ${row[0][1].toFixed(1)}`);
        }
      };
      ground();
      const clip = () => [...p.querySelectorAll('.hs p')].filter(e => e.scrollWidth > e.clientWidth).forEach(e => bad.push(`"${e.textContent.slice(0, 24)}": strip text ellipsized, ${e.scrollWidth}>${e.clientWidth}`));
      clip();
      if (V === 'herd1') {
        const f = structuredClone(data), l = f.fleets[0].lanes[0], q = f.fleets[0].queue[0];
        f.fleets[0].lanes = [...'wwwiiiiiddddddddddd'].map((s, i) => ({ ...l, name: 'audit-' + i, state: { w: 'working', i: 'idle', d: 'done' }[s] }));
        f.fleets[0].queue = Array.from({ length: 13 }, (_, i) => ({ ...q, issue: 900 + i, blocked_by: i % 3 ? null : 2683 })); // #114: mixed rows
        herdData(f);
        clip();
        ground();
      }
    }
    return { view: V, controls: ctl.length, outliers: [...early.splice(0), ...bad] };
  }
  // #65: a live update never moves the scroll or replaces a kept node. Scroll to 400 px, push a changed fixture
  // through the live path (herdData / frag), and count what the MutationObserver saw leave the tree for good.
  function gone(root, run, sel) {
    const mo = new MutationObserver(() => {}), out = new Set();
    mo.observe(root, { childList: true, subtree: true });
    run();
    for (const r of mo.takeRecords()) for (const n of r.removedNodes) if (n.nodeType === 1) for (const e of [n, ...n.querySelectorAll(sel)]) if (e.matches(sel) && !e.isConnected) out.add(e);
    mo.disconnect();
    return out.size;
  }
  const SCROLL = {
    'sheet-drag': () => ({ view: V, controls: 1, outliers: dragged }),
    toast: () => {
      const bad = [];
      if (toasted !== 'moved #12 before #18') bad.push(`toast after the POST: ${toasted}`);
      if ($('.msg.g, .msg.r')) bad.push('.msg still shown 5 s after the POST');
      if (/[?&]msg=/.test(location.search)) bad.push(`location.search keeps msg: ${location.search}`);
      return { view: V, controls: 0, outliers: bad };
    },
    'herd-scroll': () => {
      const base = structuredClone(data), bad = [], f = base.fleets[0];
      // Tall enough to scroll 400 px: the first fleet's lanes ×3.
      f.lanes.slice().concat(f.lanes).forEach((l, i) => f.lanes.push({ ...l, name: 'audit-' + i }));
      herdData(base);
      const p = hp(0), next = structuredClone(base), l = next.fleets[0].lanes;
      p.scrollTo({ top: 400, behavior: 'instant' });
      const top = p.scrollTop, keep = new Map([...p.querySelectorAll('.hh, .hqh')].map(e => [e.dataset.agent || e.dataset.i, e]));
      if (top !== 400) bad.push(`Herd page 1 scrolled to ${top}, not 400`);
      l[1].age = '1m'; // a label changes
      Object.assign(l[0], { state: 'working', shown: 'working' }); // a horse changes band
      l.splice(2, 1); // a lane goes
      l.push({ ...l[0], name: 'audit-new' }); // one comes
      next.fleets[0].queue.shift(); // a queued issue goes
      const n = gone(p, () => herdData(next), '.hh, .hqh');
      if (p.scrollTop !== top) bad.push(`Herd page 1 scrollTop ${top} -> ${p.scrollTop} on a live update`);
      if (n !== 2) bad.push(`${n} horse nodes removed, 2 agents/issues left`);
      for (const [k, e] of keep) if (p.querySelector(`[data-agent="${k}"], [data-i="${k}"]`) !== e && (l.some(x => x.name === k) || next.fleets[0].queue.some(q => String(q.issue) === k))) bad.push(`${k}: horse replaced, not patched`);
      return { view: V, controls: keep.size, outliers: bad };
    },
    'queue-scroll': () => {
      const q = $('#queue'), x = q.cloneNode(true), rows = x.querySelectorAll('details.q'), bad = [];
      scrollTo({ top: 400, behavior: 'instant' });
      const top = scrollY, keep = [...q.querySelectorAll('details')];
      if (top !== 400) bad.push(`Queue scrolled to ${top}, not 400`);
      rows[0].querySelector('summary .m').textContent = '1m'; // a row changes
      rows[rows.length - 1].remove(); // a row goes
      const n = gone(q, () => frag('queue', x.innerHTML), 'details');
      if (scrollY !== top) bad.push(`Queue scrollY ${top} -> ${scrollY} on a live update`);
      if (n !== 1) bad.push(`${n} rows removed, 1 left`);
      if (keep.filter(d => !d.isConnected).length !== 1) bad.push('a kept row was replaced, not patched');
      if (keep.some(d => d.isConnected && d.matches('.q') && !d.open)) bad.push('an open row closed');
      return { view: V, controls: keep.length, outliers: bad };
    },
  };
  const go = async () => {
    await VIEWS[V]();
    setTimeout(() => {
      if (V === 'herd2') PG.scrollLeft = PG.clientWidth;
      setTimeout(() => { if (DBG.get('check')) check(); }, 1000);
    }, V === 'toast' ? 4500 : 2000);
  };
  if (document.readyState === 'complete') go(); else addEventListener('load', go);
})();

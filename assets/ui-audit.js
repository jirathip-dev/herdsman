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
    pair: 'pair this device first: `herdsman serve pair` on the Mac',
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
    host: async () => { // the live Host tab's Alerts (the export leaves it out) as the Home Screen app shows it: an error, then unpaired
      tab('host');
      const push = Object.assign(document.createElement('button'), { id: 'push', type: 'button', textContent: 'Enable alerts' });
      push.dataset.key = 'AA';
      $('#host').append(Object.assign(document.createElement('h2'), { textContent: 'Alerts' }), push);
      tidy($('#host'));
      window.Notification = { requestPermission: async () => 'granted' };
      const sub = { toJSON: () => ({ endpoint: 'e', keys: { p256dh: 'p', auth: 'a' } }) };
      Object.defineProperty(navigator, 'serviceWorker', { value: { ready: Promise.resolve({ pushManager: { subscribe: async () => sub } }) } });
      await canned(400, LONG.sub, () => $('#push').click());
      mid('alerts error');
      await canned(401, LONG.pair, () => $('#push').click());
      $('#push').hidden = false;
      $('#push').scrollIntoView({ block: 'center', behavior: 'instant' });
    },
    events: () => tab('events'),
    herd1: () => herd(true),
    herd2: () => herd(true),
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
  const ROOTS = { sheet: () => [$('#sheet')], herd1: () => [hp(0), $('.hx'), $('.hwx')], herd2: () => [hp(1), $('.hx'), $('.hwx')] };
  const CTL = 'button:not(.lr,.hh,.hqh), .pk, [role=button]', ROW = 'summary, .lr, .hh, .hqh', NAME = '.nw, .hl b';
  const CHOICE = '.ask button[name=choice], .ask .pick'; // amendment A: wraps, ≤ 4 lines, instead of 28 px
  // A closed <details> keeps its content's boxes (content-visibility), so ask the browser what is shown.
  const seen = e => e.checkVisibility({ visibilityProperty: true });
  const label = e => (e.getAttribute('aria-label') || e.textContent).trim().replace(/\s+/g, ' ').slice(0, 24) || e.tagName.toLowerCase();
  function check() {
    let res;
    try { res = (SCROLL[V] || measure)(); } catch (e) { res = { view: V, controls: 0, outliers: ['audit error: ' + e.message] }; }
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
    const se = document.scrollingElement;
    if (se.scrollWidth > se.clientWidth) bad.push(`page scrolls sideways: ${se.scrollWidth}>${se.clientWidth}`);
    if (V.startsWith('herd')) {
      const p = hp(+V.slice(4) - 1), pr = p.getBoundingClientRect();
      for (const e of [p.querySelector('.hs'), $('.hx')]) if (e.offsetHeight !== 28) bad.push(`${e.className}: Herd strip/[×] ${e.offsetHeight}px, not 28`);
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
      const clip = () => [...p.querySelectorAll('.hs p')].filter(e => e.scrollWidth > e.clientWidth).forEach(e => bad.push(`"${e.textContent.slice(0, 24)}": strip text ellipsized, ${e.scrollWidth}>${e.clientWidth}`));
      clip();
      if (V === 'herd1') {
        const f = structuredClone(data), l = f.fleets[0].lanes[0], q = f.fleets[0].queue[0];
        f.fleets[0].lanes = [...'wwwiiiiiddddddddddd'].map((s, i) => ({ ...l, name: 'audit-' + i, state: { w: 'working', i: 'idle', d: 'done' }[s] }));
        f.fleets[0].queue = Array.from({ length: 13 }, (_, i) => ({ ...q, issue: 900 + i }));
        herdData(f);
        clip();
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

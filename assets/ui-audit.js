// #63 gate 2 (docs/ui.md), shipped only in the `serve --export --demo` page: `?audit=<view>` sets up one view
// for scripts/ui-audit.sh; `&check=1` then measures it at 390 px and writes {"view","controls","outliers"} into
// <pre id=audit>. Rows (summary, lane rows, horses) are 44 px tap rows, not 28 px controls.
(() => {
  const V = DBG.get('audit'); // serve.js read the query before it tidied the URL
  if (!V) return;
  const hp = n => H.querySelectorAll('.hp')[n];
  const VIEWS = {
    queue: () => tab('queue'),
    'queue-open': () => { tab('queue'); const d = $('#queue details.q'); d.open = true; d.scrollIntoView(); },
    lanes: () => tab('lanes'),
    sheet: () => { tab('lanes'); open($('.lane')); },
    host: () => tab('host'),
    events: () => tab('events'),
    herd1: () => herd(true),
    herd2: () => herd(true),
    // #67: the window navigated to the fetch-only `frags` lands on the board (the SW answers with the page).
    frags: () => landed() ? tab('queue') : navigator.serviceWorker.ready.then(() => navigator.serviceWorker.controller || new Promise(r => navigator.serviceWorker.addEventListener('controllerchange', r, { once: true }))).then(() => location.replace('frags' + location.search)),
  };
  const landed = () => location.pathname.endsWith('/frags');
  const ROOTS = { sheet: () => [$('#sheet')], herd1: () => [hp(0), $('.hx'), $('.hwx')], herd2: () => [hp(1), $('.hx'), $('.hwx')] };
  const CTL = 'button:not(.lr,.hh,.hqh), .pk, [role=button]', ROW = 'summary, .lr, .hh, .hqh', NAME = '.nw, .hl b';
  const CHOICE = '.ask button[name=choice], .ask .pick'; // amendment A: wraps, ≤ 4 lines, instead of 28 px
  // A closed <details> keeps its content's boxes (content-visibility), so ask the browser what is shown.
  const seen = e => e.checkVisibility({ visibilityProperty: true });
  const label = e => (e.getAttribute('aria-label') || e.textContent).trim().replace(/\s+/g, ' ').slice(0, 24) || e.tagName.toLowerCase();
  function check() {
    let res;
    try { res = measure(); } catch (e) { res = { view: V, controls: 0, outliers: ['audit error: ' + e.message] }; }
    const out = document.createElement('pre');
    out.id = 'audit';
    out.textContent = JSON.stringify(res);
    B.append(out);
  }
  function measure() {
    $$('[inert]').forEach(e => { e.inert = false; });
    const roots = (ROOTS[V] || (() => [B]))(), bad = [];
    if (V === 'frags' && !(landed() && $('#queue'))) bad.push('navigating to frags did not land on the board');
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
      for (const e of [...r.querySelectorAll(NAME)].filter(seen)) {
        const cs = getComputedStyle(e), h = e.getBoundingClientRect().height;
        const lh = cs.lineHeight === 'normal' ? 1.2 * parseFloat(cs.fontSize) : parseFloat(cs.lineHeight);
        if (h > 2 * lh + 1) bad.push(`${label(e)}: name ${h | 0}px tall, over 2 lines`);
      }
    }
    const se = document.scrollingElement;
    if (se.scrollWidth > se.clientWidth) bad.push(`page scrolls sideways: ${se.scrollWidth}>${se.clientWidth}`);
    if (V.startsWith('herd')) {
      for (const e of [hp(+V.slice(4) - 1).querySelector('.hs'), $('.hx')]) if (e.offsetHeight !== 28) bad.push(`${e.className}: Herd strip/[×] ${e.offsetHeight}px, not 28`);
    }
    return { view: V, controls: ctl.length, outliers: bad };
  }
  const go = () => {
    VIEWS[V]();
    setTimeout(() => {
      if (V === 'herd2') PG.scrollLeft = PG.clientWidth;
      setTimeout(() => { if (DBG.get('check')) check(); }, 1000);
    }, 2000);
  };
  if (document.readyState === 'complete') go(); else addEventListener('load', go);
})();

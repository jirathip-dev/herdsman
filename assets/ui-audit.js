// #63 gate 2 (docs/ui.md), shipped only in the `serve --export --demo` page: `?audit=<view>` sets up one view
// for scripts/ui-audit.sh; `&check=1` then measures it at 390 px and writes {"view","controls","outliers"} into
// <pre id=audit>. Rows (summary, lane rows, horses) are 44 px tap rows, not 28 px controls.
(() => {
  const V = DBG.get('audit'); // serve.js read the query before it tidied the URL
  if (!V) return;
  // #566: `&shot=1` (the screenshot runs) freezes motion at one frame: a frame callback runs once, at t = 2 s, and the
  // next frame it asks for never comes. The herd's endless loop (its storm) hung headless Chrome before the shot.
  let inside = false;
  if (DBG.has('shot')) window.requestAnimationFrame = f => inside ? 0 : setTimeout(() => { inside = true; try { f(2000); } finally { inside = false; } });
  const hp = n => H.querySelectorAll('.hp')[n];
  // #440: Host and Events are System's Hosts & health and Activity, opened over the Queue tab.
  const sys = s => { tab('queue'); sysopen(s); };
  // #68: each inline control's failure path with a canned long reply (the old 401 pair text, a 140-char refusal,
  // a send error); measured after each step (`mid`), then with the rest at the end. #501: a canned 401's `pair/status`
  // confirms unpaired, as the board answers an unpaired browser.
  const F = window.fetch, early = [], wait = () => new Promise(r => setTimeout(r, 300));
  const LONG = {
    pair: 'pair this device: tap Pair this phone, then `drover serve approve <code>` on the Mac',
    sub: 'push subscribe refused: the endpoint https://web.push.apple.com/QGx2c3Rlc3Qtc3Vic2NyaXB0aW9uLWVuZHBvaW50LWlk is not https or not a known push service',
    kill: 'REFUSED UNSAFE_CLEAN: acme-web-impl-48-contrast-audit-for-every-settings-pane-abxx has 3 unpushed commits and a dirty tree; push them first',
    dry: 'dry run; would push 3 commits and 2 dirty files to wip/acme-web-impl-48-contrast-audit-for-every-settings-pane-abxx-20261031-1943, then close pane w1:p5',
    send: 'error: herdr pane send w1:p5 failed: pane closed while sending (exit 1); the message was not delivered, nothing was typed into the agent',
  };
  const canned = async (status, text, then) => {
    window.fetch = (u, o) => o && o.method === 'POST' ? Promise.resolve(new Response(text, { status }))
      : status === 401 && String(u).endsWith('pair/status') ? Promise.resolve(Response.json({ paired: false })) : F(u, o);
    then();
    await wait();
  };
  const mid = what => early.push(...measure().outliers.map(o => `${what}: ${o}`));
  // #234 gate 2: the Host groups in their fixed order (HOSTS, FLEET DISK, SERVICES, FLEETS, ALERTS), main's card first;
  // one tab is active, its label on the accent fill.
  const ranked = () => {
    const t = [...$$('#host>.grp .gn')].map(g => g.textContent).join(' ');
    if (!/^HOSTS FLEET DISK SERVICES( FLEETS)?$/.test(t)) early.push(`Host groups not in order: ${t}`);
    if (!$('#host .hcd .tag')?.closest('.hcd').matches(':first-child')) early.push('the main host is not the first card');
    const on = $$('nav a.on'), probe = B.appendChild(Object.assign(document.createElement('i'), { style: 'color:var(--l)' }));
    const accent = getComputedStyle(probe).color;
    probe.remove();
    if (on.length !== 1) early.push(`${on.length} active tabs, not 1`);
    else if (getComputedStyle(on[0].firstElementChild).backgroundColor !== accent) early.push(`the active tab's fill is ${getComputedStyle(on[0].firstElementChild).backgroundColor}, not the accent ${accent}`);
    // #183: every Host label is whole on one line, ≥ 1ch before its value, and every value starts at one x (groups
    // opened for the measure, then put back).
    const gs = [...$$('#host>.grp')], was = gs.map(g => g.open), xs = new Map();
    gs.forEach(g => { g.open = true; });
    for (const k of $$('#host .k')) {
      const g = document.createRange(), v = k.nextElementSibling, ch = k.parentNode.appendChild(Object.assign(document.createElement('i'), { style: 'position:absolute;width:1ch' }));
      g.selectNodeContents(k);
      const lines = new Set([...g.getClientRects()].map(r => Math.round(r.top))).size, end = g.getBoundingClientRect().right, x = v.getBoundingClientRect().left, w = ch.getBoundingClientRect().width;
      ch.remove();
      if (lines !== 1 || k.scrollWidth > k.clientWidth) early.push(`Host label "${k.textContent}": ${lines} lines, ${k.scrollWidth}px of text in ${k.clientWidth}px`);
      if (x - end < w - 0.5) early.push(`Host label "${k.textContent}": its value ${(x - end).toFixed(1)}px after it, under 1ch (${w.toFixed(1)}px)`);
      xs.set(k.textContent, Math.round(x));
    }
    if (new Set(xs.values()).size > 1) early.push(`Host values start at different x: ${[...xs].map(([k, x]) => `${k} ${x}`).join(', ')}`);
    gs.forEach((g, i) => { g.open = was[i]; });
  };
  // #173: a fleet's [config] (#440: its row in System's Fleets), scrolled to its Pipeline group.
  const pipe = async fleet => {
    tab('queue');
    const b = [...$$('#queue .cfg')].find(b => b.dataset.f === fleet);
    if (!b) return early.push(`no [config] button for ${fleet} in the demo`);
    b.click();
    const g = [...$$('.sfl')].find(d => d.dataset.f === fleet && d.open)?.querySelector('.pp');
    if (!g?.querySelector(`.pg[aria-label^="${fleet} "]`)) return early.push(`no pipeline graph for ${fleet} in the demo`);
    g.scrollIntoView();
  };
  // #403: thread t3 open, its composer as a paired phone has it (the demo's is inert).
  const composer = () => {
    tab('chat'); chatOpen('t3'); $('.lall')?.click();
    const f = $('#chat .tw[data-t=t3]');
    f.querySelectorAll('[disabled]').forEach(e => { e.disabled = false; });
    return f;
  };
  // #407: t3 mid-turn (its live line as serve.rs renders it, the turn 83 s old), with `qd` queued messages above the box
  // (their markup as serve.rs renders it), its composer enabled.
  const midTurn = (qd = '') => {
    tab('chat'); chatOpen('t3'); $('.lall')?.click();
    const c = $('#chat').cloneNode(true), l = c.querySelector('.cvp[data-t=t3] .tlv');
    Object.assign(l, { className: 'tlv wk', innerHTML: '<i class=tld aria-hidden=true></i> working' }).dataset.since = Math.round(Date.now() / 1e3) - 83;
    c.querySelector('.tw[data-t=t3] input[type=file]').insertAdjacentHTML('beforebegin', qd);
    frag('chat', c.innerHTML);
    return composer();
  };
  const QD = (q, t) => `<div class=tq data-q=${q}><p title="${t}">${t}</p><span class=btns><button type=button name=qn>send now</button><button type=button name=qe>edit</button><button type=button name=qx>remove</button></span></div>`;
  const pillIs = (want, what) => { const p = $('#chat .cvp[data-t=t3] .pill'), t = p && [...p.childNodes].filter(n => !n.matches?.('.tld')).map(n => n.textContent).join(''); if (!p || !seen(p) || !want.test(t)) early.push(`${what}: pill "${t}"`); return p; };
  // A long press on `e` (a held ½ s timer fired by hand: virtual time).
  const press = e => {
    const r = e.getBoundingClientRect(), st = window.setTimeout, fs = [];
    window.setTimeout = (f, ms) => ms === 500 ? fs.push(f) : st(f, ms);
    e.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: r.left + 10, clientY: r.top + 10 }));
    window.setTimeout = st;
    fs.forEach(f => f());
    e.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, clientX: r.left + 10, clientY: r.top + 10 }));
    e.click(); // the click a release ends with: it does nothing else
  };
  const folded = () => {
    const cards = [...$$('#needs>.ask')];
    if (cards.length < 3) return early.push(`${cards.length} needs-you cards in the demo, not 3`), false;
    for (const c of cards) {
      const s = c.querySelector(':scope>summary'), t = s?.firstElementChild;
      if (c.open || !t || !seen(s) || t.offsetHeight > 1.5 * parseFloat(getComputedStyle(t).lineHeight) || getComputedStyle(t).textOverflow !== 'ellipsis') early.push(`needs card not one folded line: ${c.textContent.slice(0, 40)}`);
    }
    return true;
  };
  const VIEWS = {
    queue: () => { tab('queue'); if ([...$$('#needs>.ask, .later:has(+#needs)')].some(seen)) early.push('a needs-you card on Queue'); },
    // #281 gate 2: the Queue and Events rows in the shot (no needs cards above them, in the check too), so their sprites' pixels are checked.
    'queue-rows': () => { tab('queue'); $('#needs').hidden = true; },
    'events-rows': () => { sys('activity'); $('#needs').hidden = true; },
    // #266 gate 2: the first [important] row open (links, time, the raw event), and the [all] view.
    'events-open': () => { sys('activity'); $('#needs').hidden = true; $('#events [data-v=imp] .er').open = true; },
    'events-all': () => { sys('activity'); $('#needs').hidden = true; $('#events .chips [data-k=all]').click(); },
    'queue-open': () => { tab('queue'); const d = $('#queue details.q'); d.open = true; d.scrollIntoView(); },
    // #205: acme-web #38's open card lists its 3 lanes newest first: one line each (ellipsis), a 44 px row; gone ones are no link.
    // #218: renamed to real-length names (≥ 30 chars), moved to the top: the name ellipsises inside the card, the age stays
    // in view, the sections read STATUS, LINKS, LANES (3); a tap on a lane no Lanes row lists opens the sheet by its name.
    'queue-lanes': async () => {
      tab('queue');
      const d = [...$$('#queue details.q')].find(d => d.querySelector('summary a')?.textContent === '#38');
      if (!d) return early.push('no acme-web #38 row in the demo');
      $('#queue').prepend(d);
      if (!DBG.get('check')) $('#needs').hidden = true;
      d.open = true;
      const ls = [...d.querySelectorAll('.qls>.ql')], long = ['drover-impl-157-queue-card-lane-overflow', 'acme-rev-2940-r1-settings-contrast-audit', 'drover-impl-205-queue-card-lanes-newest-first'];
      if (ls.length !== 3) early.push(`#38 lists ${ls.length} lanes, not 3`);
      ls.forEach((l, i) => { l.querySelector('.qn').textContent = long[i]; if (l.dataset.agent) l.dataset.agent = long[i]; });
      const hs = [...d.querySelectorAll('.cd>.ch')].map(h => h.textContent).join(', ');
      if (hs !== 'STATUS, LINKS, LANES (3)') early.push(`#38 sections "${hs}", not STATUS, LINKS, LANES (3)`);
      for (const l of ls) {
        const n = l.querySelector('.qn'), a = l.querySelector('.qa'), r = l.getBoundingClientRect();
        if (r.height !== 44 || getComputedStyle(n).textOverflow !== 'ellipsis' || n.scrollWidth <= n.clientWidth) early.push(`lane "${l.textContent}": ${r.height}px, not one 44 px line, its name cut by an ellipsis`);
        if (a.getBoundingClientRect().right > r.right + 1 || a.scrollWidth > a.clientWidth) early.push(`lane "${l.textContent}": its age pushed out of the line`);
      }
      if (ls.some(l => l.textContent.includes(' gone ') && l.matches('a:not([data-gone])'))) early.push('a gone lane is a link');
      // The tap: the Lanes tab never lists this name, its transcript read answers; the sheet opens on it.
      const q = ls.find(l => l.matches('a')), name = q?.dataset.agent;
      window.fetch = u => String(u).includes(encodeURIComponent(name)) ? Promise.resolve(new Response('{"harness":"claude","lines":[],"live":0}')) : F(u);
      q?.click();
      await wait();
      window.fetch = F;
      if (!shown() || $('.hd b').textContent !== name) early.push(`tap on ${name}: sheet ${shown() ? 'shows ' + $('.hd b').textContent : 'not opened'}`);
      close();
      await wait();
    },
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
    // #92: the demo's `driver = "drover"` fleet (docs): its card with the `drive · …` line and [drive now], moved to the
    // top (a scrolled view shoots blank).
    'queue-drive': () => {
      tab('queue');
      $('#queue').prepend([...$$('#queue .fc')].find(c => c.querySelector('.dp li details'))); // the fleet with a pass, not `drive · no pass yet`
    },
    // #162: the [config] sheet of the fleet that needs you (acme-api): every group, the role pickers.
    config: () => { tab('queue'); $('#queue .cfg').click(); },
    // #166: the driven fleet's result line in its 3 colours: the demo's amber pass (open, an action's code open), then
    // green and red copies under it, its card moved to the top; each must read in its colour.
    'queue-result': () => {
      tab('queue');
      const d = [...$$('#queue details.dp')].find(d => d.querySelector('li details'));
      if (!d) return early.push('no drive result line in the demo');
      const copy = (tone, text) => { const c = d.cloneNode(true); c.className = 'dp ' + tone; c.querySelector('summary>span').textContent = text; return c; };
      const all = [d, copy('g', 'drive · 4m ago · started 1 · merged 1'), copy('r', 'drive · 2m ago · started 0 · merged 0 · 1 failed')];
      d.after(...all.slice(1));
      $('#queue').prepend(d.closest('.fc'));
      d.open = true;
      d.querySelector('li details').open = true;
      if (!DBG.get('check')) $('#needs').hidden = true; // the shot shows the lines; the check keeps the needs cards
      for (const c of all) {
        const tone = c.classList[1], probe = B.appendChild(Object.assign(document.createElement('i'), { className: tone })), want = getComputedStyle(probe).color;
        probe.remove();
        if (getComputedStyle(c.querySelector('summary')).color !== want) early.push(`.dp.${tone} reads ${getComputedStyle(c.querySelector('summary')).color}, not ${want}`);
      }
    },
    // #173: the Pipeline group of a fleet's [config] sheet: the built-in one (acme-web), then acme-api's custom
    // impl → rev → sec → gates.
    pipeline: () => pipe('acme-web'),
    'pipeline-custom': () => pipe('acme-api'),
    lanes: () => { tab('lanes'); folded(); },
    'needs-fold': async () => {
      tab('lanes');
      if (!folded()) return;
      $$('[inert]').forEach(e => { e.inert = false; });
      const sent = [], grab = e => { e.preventDefault(); e.stopImmediatePropagation(); sent.push(JSON.stringify([...new FormData(e.target, e.submitter)])); };
      addEventListener('submit', grab, true);
      const mains = [...$$('#needs>.ask>summary>button')];
      if (!mains.length) early.push('no main button on a folded line');
      for (const b of mains) {
        const c = b.closest('.ask'), twin = b.form && [...b.form.elements].find(x => x !== b && x.name === b.name && x.value === b.value);
        if (!twin || !c.contains(twin)) { early.push(`${label(b)}: no twin in its card's form`); continue; }
        b.disabled = twin.disabled = false;
        b.scrollIntoView({ block: 'center', behavior: 'instant' });
        const r = b.getBoundingClientRect();
        document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2).click();
        twin.click();
        if (sent.length !== 2 || sent[0] !== sent[1]) early.push(`${label(b)}: the line's button sent ${sent[0]}, the card's ${sent[1]}`);
        if (c.open) early.push(`${label(b)}: the line's button opened the card`);
        sent.length = 0;
      }
      window.removeEventListener('submit', grab, true);
      const d = await (await fetch(new URL('api/v1/needs', document.baseURI))).json(), c = [...$$('#needs>.ask')].pop(), k = c.querySelector('summary>span').textContent;
      c.querySelector('summary').click();
      if (!c.open) early.push('a tap on the line left the card folded');
      d.asks.shift();
      d.now += 600;
      dispatchEvent(new CustomEvent('herd-needs', { detail: d }));
      const o = [...$$('#needs>.ask')].filter(x => x.open).map(x => x.querySelector('summary>span').textContent);
      if (o.join() !== k) early.push(`after a herd-needs redraw the open cards are [${o}], not [${k}]`);
      const a = [...$$('#needs>.ask')].find(x => x.open);
      a?.querySelector('summary').click();
      if (a?.open) early.push('a second tap on the line left the card open');
      a?.querySelector('summary').click();
    },
    // #386: the [chat] tab's thread list (the needs-you stack folded with [later all]), and one thread open from its
    // first message, with the choice rows, images, a file row and the composer (scrolled to the top: a scrolled view
    // shoots blank); at 1440 the list and the thread side by side.
    chat: () => { tab('chat'); chatOpen(''); $('.lall')?.click(); },
    // (#443: on a phone the messages scroll inside the thread, so those go to the top too.)
    'chat-thread': () => { tab('chat'); chatOpen('t4'); $('.lall')?.click(); scrollTo({ top: 0, behavior: 'instant' }); $$('#chat .tc').forEach(c => { c.scrollTop = 0; }); },
    // #492: t5's handoff card: acme-api's thread as `<title> · <id>` with its last topic and age, then [new thread].
    'chat-handoff': () => { tab('chat'); chatOpen('t5'); $('.lall')?.click(); scrollTo({ top: 0, behavior: 'instant' }); $$('#chat .tc').forEach(c => { c.scrollTop = 0; }); },
    // #409: t2's reply with a short and a long-line code box (the long one scrolls sideways inside its box), then
    // [copy] tapped on the short one: `copied` (the clipboard stubbed: headless Chrome's needs a permission).
    'chat-code': () => {
      tab('chat'); chatOpen('t2'); $('.lall')?.click(); scrollTo({ top: 0, behavior: 'instant' }); $$('#chat .tc').forEach(c => { c.scrollTop = 0; });
      const [a, b] = [...$$('#chat .cvp[data-t="t2"] .cb pre')];
      if (!b || a.scrollWidth > a.clientWidth || b.scrollWidth <= b.clientWidth || getComputedStyle(b).overflowX !== 'auto') early.push(`code boxes: ${a?.scrollWidth}/${a?.clientWidth}, ${b?.scrollWidth}/${b?.clientWidth}, not short whole and long scrolling`);
    },
    'chat-copied': async () => {
      VIEWS['chat-code']();
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: t => { window.cp = t; return Promise.resolve(); } } });
      const c = $('#chat .cvp[data-t="t2"] .cpy'), st = window.setTimeout;
      window.setTimeout = (f, ms) => ms === 1500 ? 0 : st(f, ms); // the flash held for the shot
      c.click();
      await wait();
      window.setTimeout = st;
      if (c.textContent !== 'copied' || window.cp !== 'curl -s localhost:8080/v1/openapi.json') early.push(`[copy]: "${c.textContent}", copied "${window.cp}"`);
    },
    // #406: the image viewer over t4 from a tap on its first image: fitted, zoomed (a double tap on its middle), and
    // several images (the thread's two, on the second).
    // (The zoom's easing is finished at once: headless Chrome's virtual time never runs it. The thread is scrolled to
    // the top first, as chat-thread: a scrolled view shoots blank, the viewer over it too.)
    'chat-viewer': () => { VIEWS['chat-thread'](); $('#chat .cvp[data-t="t4"] .im img').click(); },
    'chat-viewer-zoom': () => { VIEWS['chat-viewer'](); ivTap(innerWidth / 2, innerHeight / 2); IVI.getAnimations().forEach(a => a.finish()); },
    'chat-viewer-multi': () => { VIEWS['chat-thread'](); const l = [...$$('#chat .cvp[data-t="t4"] .im img')].map(i => [i.src, i.alt]); iv(l, l[1][0]); },
    // #403 gate 2: [send] clears the box before the POST answers and shows the message `sending…` (no #ld overlay, ever);
    // ✓ once accepted; failed, red with [retry] and the text back in the box. The shot: ✓, sending… and failed in t3.
    'chat-send': async () => {
      const f = composer(), box = f.elements.text, ld = new MutationObserver(() => { if (!$('#ld').hidden) early.push('the #ld overlay showed on a chat post'); });
      ld.observe($('#ld'), { attributes: true });
      const sent = () => [...$$('#chat .cvp[data-t=t3] .snt')].map(b => b.querySelector('.ct').textContent);
      const hold = [];
      window.fetch = (u, o) => o && o.method === 'POST' ? new Promise(r => hold.push(r)) : F(u, o);
      const say = t => { box.value = t; f.requestSubmit(); };
      say('ship the notes on Friday, with the 429 change\nand the OpenAPI link');
      if (box.value || !hold.length || sent().join() !== 'sending…') early.push(`after [send]: box "${box.value}", ${hold.length} POSTs, marks ${sent()}`);
      await wait();
      if (sent().join() !== 'sending…') early.push(`300 ms on, unanswered: ${sent()}`);
      hold[0](new Response('sent\n'));
      await wait();
      if (sent().join() !== '✓') early.push(`accepted: ${sent()}, not ✓`);
      say('and tag the release');
      say('this one fails');
      hold[2](new Response('error: herdr agent list failed: exit 1', { status: 409 }));
      await wait();
      const bad = $('#chat .snt.bad');
      if (sent().join() !== '✓,sending…,✗ not sent' || !bad?.querySelector('.rt') || box.value !== 'this one fails') early.push(`failed: marks ${sent()}, [retry] ${!!bad?.querySelector('.rt')}, box "${box.value}"`);
      if (getComputedStyle(bad.querySelector('.ct')).color !== getComputedStyle(B.appendChild(el('i', 'r'))).color) early.push('the failed mark is not red');
      $$('body>i.r').forEach(i => i.remove());
      ld.disconnect();
      if (!DBG.get('check')) { $$('#chat .cvp[data-t=t3] .tc>:not(.snt)').forEach(e => { e.hidden = true; }); $('.msg.r')?.remove(); }
    },
    // #403 gate 2: a long draft grows the box to 6 lines, then it scrolls inside; a send shrinks it back to one.
    'chat-long': async () => {
      const f = composer(), box = f.elements.text, h = () => box.getBoundingClientRect().height, lh = parseFloat(getComputedStyle(box).lineHeight);
      const type = t => { box.value = t; box.dispatchEvent(new Event('input', { bubbles: true })); return h(); };
      const one = type(''), three = type('one\ntwo\nthree'), long = type('Rate limits: return 429 with Retry-After on /v1/search, per API key, 600 a minute with a burst of 100. '.repeat(6));
      const cs = getComputedStyle(box), pad = ['paddingTop', 'paddingBottom', 'borderTopWidth', 'borderBottomWidth'].reduce((n, k) => n + parseFloat(cs[k]), 0);
      if (one > 44 || Math.abs(three - 3 * lh - pad) > 1 || Math.abs(long - 6 * lh - pad) > 1 || box.scrollHeight <= box.clientHeight) early.push(`box heights: empty ${one}, 3 lines ${three}, long ${long} (${box.scrollHeight} of text), not 3 and 6 lines of ${lh} + ${pad}`);
      window.fetch = (u, o) => o && o.method === 'POST' ? new Promise(() => {}) : F(u, o);
      if (DBG.get('check')) { f.requestSubmit(); if (h() !== one) early.push(`after [send] the box is ${h()} px, not ${one}`); }
      else $$('#chat .cvp[data-t=t3] .tc>*').forEach(e => { e.hidden = true; });
    },
    // #403 gate 2: the keyboard up (the box focused): no tab bar on a phone, the composer at the bottom.
    'chat-kb': async () => {
      const box = composer().elements.text;
      box.focus();
      box.value = 'grill me on the land mode';
      await wait();
      if (document.activeElement !== box || seen($('nav'))) early.push(`box focused ${document.activeElement === box}, tab bar shown ${seen($('nav'))}`);
    },
    // #403 gate 2: opening a thread pushes a history entry, so back returns to the list; so does [‹], and a swipe
    // right from the left edge past a third of the width (it follows the finger); a short, mid-screen or downward one does not.
    'chat-back': async () => {
      tab('chat'); chatOpen(''); $('.lall')?.click();
      const row = () => $('#chat .tr[data-t=t3]'), open = () => THR === 't3' && !seen($('#chat .clp'));
      const popped = () => new Promise(r => addEventListener('popstate', () => setTimeout(r), { once: true }));
      row().click();
      if (!open() || history.state?.thr !== 't3') early.push(`row tap: thread ${THR}, history ${JSON.stringify(history.state)}`);
      const p = popped(); history.back(); await p;
      if (THR !== '' || !seen($('#chat .clp'))) early.push(`back: thread "${THR}", not the list`);
      row().click();
      const q = popped(); $('#chat .cvp[data-t=t3] .bk').click(); await q;
      if (THR !== '') early.push(`[‹]: thread "${THR}", not the list`);
      const v = $('#chat .cvp[data-t=t3]'), swipe = (x0, y0, x1, y1, mid) => {
        const t = (x, y) => new Touch({ identifier: 1, target: v, clientX: x, clientY: y });
        v.dispatchEvent(new TouchEvent('touchstart', { bubbles: true, touches: [t(x0, y0)], changedTouches: [t(x0, y0)] }));
        v.dispatchEvent(new TouchEvent('touchmove', { bubbles: true, touches: [t((x0 + x1) / 2, (y0 + y1) / 2)], changedTouches: [t((x0 + x1) / 2, (y0 + y1) / 2)] }));
        if (mid) mid();
        v.dispatchEvent(new TouchEvent('touchend', { bubbles: true, touches: [], changedTouches: [t(x1, y1)] }));
      };
      for (const [what, x0, y0, x1, y1] of [['a short edge swipe', 4, 400, 100, 400], ['a swipe from mid-screen', 120, 400, 380, 400], ['a downward drag from the edge', 4, 300, 60, 600]]) {
        row().click();
        swipe(x0, y0, x1, y1);
        if (!open()) early.push(`${what} left the thread`);
        if (v.style.transform) early.push(`${what}: the thread did not snap back (${v.style.transform})`);
        const r = popped(); history.back(); await r;
      }
      row().click();
      const s = popped();
      swipe(4, 400, 300, 400, () => { if (v.style.transform !== 'translateX(148px)') early.push(`mid-swipe the thread is at "${v.style.transform}", not under the finger (148px)`); });
      await s;
      if (THR !== '' || !seen($('#chat .clp'))) early.push(`an edge swipe: thread "${THR}", not the list`);
    },
    // #403 gate 2: a working thread (its live line as serve.rs renders it): the overlay's art, small, above the box —
    // the theme's sprite (pixel), its ascii_loading frames (ascii, `ascii-chat-wait`), the glyph (terminal); a still
    // frame with reduced motion (`chat-wait-still`); never the #ld overlay.
    'chat-wait': () => {
      midTurn(); // #407: the mark is in the floating pill
      const i = $('#chat .cvp[data-t=t3] .pill .tld'), t = document.documentElement.dataset.theme, asc = B.dataset.mode === 'ascii';
      const want = asc ? '.lda' : t === 'terminal' ? 'i' : '.lds', shown = [...i.children].filter(seen), r = i.getBoundingClientRect();
      const still = matchMedia('(prefers-reduced-motion: reduce)').matches, a = shown[0] && getComputedStyle(asc ? shown[0].lastChild : shown[0]).animationName;
      if (shown.length !== 1 || !shown[0].matches(want)) early.push(`${t}${asc ? ' ascii' : ''}: the mark shows ${shown.map(e => e.outerHTML.slice(0, 30))}, not ${want}`);
      if (r.height > 24 || r.bottom > $('#chat .tw[data-t=t3]').getBoundingClientRect().top) early.push(`the mark ${r.height} px tall, or not above the box`);
      if (V.endsWith('still') ? a !== 'none' : a === 'none') early.push(`the mark's animation: ${a}`);
      if (seen($('#ld'))) early.push('the #ld overlay shows');
      if (!DBG.get('check')) $$('#chat .cvp[data-t=t3] .tc>*').forEach(e => { e.hidden = true; });
    },
    'chat-wait-still': () => VIEWS['chat-wait'](),
    // #407 gate 2: after a send the message sits 16 px under the thread bar, scrolled to the end, the room under it
    // empty until the reply fills it; a reply that comes keeps it there.
    'chat-pin': async () => {
      const f = composer(), box = f.elements.text;
      window.fetch = (u, o) => o && o.method === 'POST' ? new Promise(() => {}) : F(u, o);
      box.value = 'and the premerge checks: which ones block?';
      f.requestSubmit();
      await wait();
      const at = () => $('#chat .cvp[data-t=t3] .snt').getBoundingClientRect().top - Math.max(0, ...[$('header'), $('#chat .cvp[data-t=t3] .th')].map(e => e.getBoundingClientRect().bottom));
      const c = innerWidth < 1000 ? $('#chat .cvp[data-t=t3] .tc') : document.scrollingElement, end = () => Math.abs(c.scrollTop + c.clientHeight - c.scrollHeight) <= 1; // (#443: a phone's messages scroll)
      if (Math.abs(at() - 16) > 1 || !end()) early.push(`pinned: ${at()} px under the bar (or the screen's top), not 16; at the end ${end()}`);
    },
    // #407 gate 2: a finished turn's commands fold into one `2 commands` row (t4), then open: its rows under it.
    'chat-fold': () => {
      VIEWS['chat-thread']();
      const d = [...$$('#chat .cvp[data-t=t4] .fold')].find(d => d.querySelector('summary').textContent === '2 commands');
      if (!d || d.open || d.querySelector('summary').textContent !== '2 commands' || d.querySelectorAll('.blk.run').length !== 2) early.push(`no closed "2 commands" fold: ${d?.outerHTML.slice(0, 80)}`);
      if (!d) return;
      [...$$('#chat .cvp[data-t=t4] .tc>*')].slice(0, [...d.parentNode.children].indexOf(d) - 1).forEach(e => { e.hidden = true; });
      FOL = false; // read from the top, as after a drag up (a scrolled view shoots blank)
      scrollTo({ top: 0, behavior: 'instant' }); $$('#chat .tc').forEach(c => { c.scrollTop = 0; });
      if (DBG.get('check')) { d.open = true; if (!seen(d.querySelector('.blk.run'))) early.push('the open fold shows no rows'); }
    },
    // #407 gate 2: the pill: `working 1m 23s` with the art and [stop]; after [stop] `stopping…`; with the stream down
    // `reconnecting…`, then (30 s or offline) `offline · tap to retry`; idle and connected, none.
    'chat-pill': () => {
      if (DBG.get('check')) { composer(); if ($('#chat .pill')) early.push('an idle thread shows a pill'); }
      midTurn();
      const p = pillIs(/^working 1m 2\ds?stop$/, 'working');
      if (!p?.querySelector('.tld').children.length || !p.querySelector('.stp')) early.push('the working pill has no art or [stop]');
      if (p && p.getBoundingClientRect().bottom > $('#chat .tw[data-t=t3] textarea').getBoundingClientRect().top) early.push('the pill is not above the box');
    },
    'chat-pill-stopping': () => {
      VIEWS['chat-pill']();
      window.fetch = (u, o) => o && o.method === 'POST' ? new Promise(() => {}) : F(u, o);
      $('#chat .pill .stp').click();
      pillIs(/^stopping…$/, 'after [stop]');
    },
    'chat-pill-reconnecting': () => {
      VIEWS['chat-pill']();
      delete B.dataset.demo;
      down = Date.now() - 5000;
      pill();
      pillIs(/^reconnecting…$/, 'the stream down 5 s');
    },
    'chat-pill-offline': () => {
      VIEWS['chat-pill']();
      delete B.dataset.demo;
      down = Date.now() - 40000;
      pill();
      pillIs(/^offline · tap to retry$/, 'the stream down 40 s');
    },
    // #407 gate 2: mid-turn [send] reads [queue] with its hint; queued messages wait above the box, each with
    // [send now], edit, [remove]. A send queues (op=queue, no `sending…` mark).
    'chat-queue': async () => {
      const f = midTurn(QD(101, 'and run the premerge checks on staging first') + QD(102, 'then tell me which ones would have blocked #48, with the logs of each failing one'));
      const posts = [];
      window.fetch = (u, o) => o && o.method === 'POST' ? (posts.push(Object.fromEntries(new URLSearchParams(o.body))), Promise.resolve(new Response('queued\n'))) : F(u, o);
      const rows = [...f.querySelectorAll('.tq')], top = f.elements.text.getBoundingClientRect().top;
      if (f.querySelector('.snd').textContent !== 'queue' || !f.querySelector('.qh')) early.push(`[send] reads "${f.querySelector('.snd').textContent}", hint ${!!f.querySelector('.qh')}`);
      if (rows.length !== 2 || rows.some(r => r.getBoundingClientRect().bottom > top || r.querySelectorAll('button').length !== 3)) early.push('the queued rows are not above the box with 3 buttons each');
      if (DBG.get('check')) {
        f.elements.text.value = 'one more';
        f.requestSubmit();
        await wait();
        if (posts[0]?.op !== 'queue' || posts[0]?.text !== 'one more' || $('#chat .snt')) early.push(`a mid-turn send: ${JSON.stringify(posts[0])}, a sending mark ${!!$('#chat .snt')}`);
      }
    },
    // #407 gate 2: edit on a queued message: its text in the box, `editing queued message · [cancel]`, [send]; its send
    // replaces it (op=queue, q) and the draft comes back; [cancel] puts the draft back too.
    'chat-queue-edit': async () => {
      const f = midTurn(QD(101, 'and run the premerge checks on staging first')), box = f.elements.text, posts = [];
      window.fetch = (u, o) => o && o.method === 'POST' ? (posts.push(Object.fromEntries(new URLSearchParams(o.body))), Promise.resolve(new Response('queued\n'))) : F(u, o);
      box.value = 'my draft';
      f.querySelector('.tq button[name=qe]').click();
      if (box.value !== 'and run the premerge checks on staging first' || f.querySelector('.qe')?.textContent !== 'editing queued message · cancel' || f.querySelector('.snd').textContent !== 'send') early.push(`editing: box "${box.value}", line "${f.querySelector('.qe')?.textContent}"`);
      if (DBG.get('check')) {
        f.querySelector('.ecl').click();
        if (box.value !== 'my draft' || f.querySelector('.qe')) early.push(`[cancel]: box "${box.value}"`);
        f.querySelector('.tq button[name=qe]').click();
        box.value = 'run the premerge checks on main too';
        f.requestSubmit();
        await wait();
        if (posts[0]?.op !== 'queue' || posts[0]?.q !== '101' || box.value !== 'my draft') early.push(`edit sent: ${JSON.stringify(posts[0])}, box "${box.value}"`);
      }
    },
    // #407 gate 2: a long press on [queue] offers [queue] and [steer], each with its line; [steer] posts op=steer.
    'chat-steer': async () => {
      const f = midTurn(), posts = [];
      window.fetch = (u, o) => o && o.method === 'POST' ? (posts.push(Object.fromEntries(new URLSearchParams(o.body))), Promise.resolve(new Response('sent\n'))) : F(u, o);
      f.elements.text.value = 'stop: the checks run on main, not staging';
      press(f.querySelector('.snd'));
      const m = [...$$('#mm button')].map(b => b.textContent).join(' | ');
      if (!seen($('#mm')) || m !== 'queue · delivered when this turn ends | steer · stops the turn and sends it now') early.push(`the send menu: ${m}`);
      if (posts.length) early.push('the long press sent');
      if (DBG.get('check')) { $('#mm button[value=steer]').click(); await wait(); if (posts[0]?.op !== 'steer' || !$('#chat .snt')) early.push(`[steer]: ${JSON.stringify(posts[0])}`); }
    },
    // #407 gate 2: a long press on a message: [copy] [copy as markdown] [quote]; copy puts its text, markdown its
    // source, then `copied`; quote puts `> ` lines in the box.
    'chat-menu': async () => {
      VIEWS['chat-code']();
      let cp = null;
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: t => { cp = t; return Promise.resolve(); } } });
      const b = $('#chat .cvp[data-t=t2] .blk.ai .md code')?.closest('.blk') || $('#chat .cvp[data-t=t2] .blk.ai');
      press(b);
      const m = [...$$('#mm button')].map(x => x.textContent).join(' | ');
      if (m !== 'copy | copy as markdown | quote') early.push(`the message menu: ${m}`);
      if (DBG.get('check')) {
        const src = b.querySelector('.md').dataset.md, box = $('#chat .tw[data-t=t2] textarea'), st = window.setTimeout;
        window.setTimeout = (f, ms) => ms === 1000 ? 0 : st(f, ms);
        $('#mm button[value=md]').click(); await wait();
        if (cp !== src || $('#mm').textContent !== 'copied') early.push(`copy as markdown: "${cp?.slice(0, 30)}", menu "${$('#mm').textContent}"`);
        press(b); $('#mm button[value=copy]').click(); await wait();
        if (cp.includes('```') || cp.includes('`') || !cp.includes('curl -s localhost:8080/v1/openapi.json')) early.push(`copy: "${cp.slice(0, 40)}"`);
        press(b); $('#mm button[value=quote]').click();
        if (!box.value.startsWith('> ') || seen($('#mm'))) early.push(`quote: box "${box.value.slice(0, 30)}"`);
        window.setTimeout = st; // (the menu closed: it would cover the rows the tap check measures)
      }
    },
    // #407 gate 2: [archive] after a swipe: `archived · [undo]` for 5 s (held for the shot); [undo] posts restore.
    'chat-undo': async () => {
      VIEWS['chat-archive']();
      const posts = [], st = window.setTimeout;
      window.fetch = (u, o) => o && o.method === 'POST' ? (posts.push(Object.fromEntries(new URLSearchParams(o.body))), Promise.resolve(new Response(posts.length > 1 ? 't4 restored\n' : 't4 archived\n'))) : F(u, o);
      window.setTimeout = (f, ms) => ms === 5000 ? 0 : st(f, ms);
      $('#chat .sw.op .sb[name=archive]').click();
      await wait();
      window.setTimeout = st;
      const m = $('.msg.g');
      if (m?.textContent !== 'archived · undo' || !m.querySelector('button.und')) early.push(`after [archive]: toast "${m?.textContent}"`);
      if (DBG.get('check')) { m.querySelector('.und').click(); await wait(); if (posts[1]?.op !== 'restore' || posts[1]?.t !== 't4') early.push(`[undo]: ${JSON.stringify(posts[1])}`); }
    },
    'ascii-chat-wait': () => { style('ascii'); return VIEWS['chat-wait'](); },
    // #408: a long thread: the demo's t4 under an older-messages marker, the page before it 30 canned messages. `chat-older`:
    // scrolled to the top, that page loaded above (the message on screen stays put); `chat-mid`: then half way down, the
    // thread header pinned under the page header.
    'chat-older': async () => {
      tab('chat'); chatOpen('t4'); $('.lall')?.click();
      const tc = $('#chat .cvp[data-t="t4"] .tc'), a = tc.querySelector(':scope>[data-n]'), at = Date.now() / 1e3 | 0;
      tc.firstElementChild.outerHTML = `<p class="qs tmo" data-b=${a.dataset.n}>↑ older messages</p>`;
      const msg = i => `<details data-n=${i} class="blk ${i % 3 ? 'ai' : 'me'}" open><summary><span class=k>${i % 3 ? 'conductor' : 'you'}</span><span class=pv>older message ${i}</span><span class=ct data-ts=${at}></span><i class=cv></i></summary><div class=md><p>older message ${i}: the rate limit's burst, the retry window and the docs page.</p></div></details>`;
      const page = '<p class=qs>thread started by owner · 09:12</p>' + Array.from({ length: 30 }, (_, i) => msg(i + 1)).join('');
      window.threadOlder = t => Promise.resolve(t === 't4' ? page : null); // #269: the board's read of API v1 `thread/t4?before=`
      // (#443: on a phone only the messages scroll, never the page.)
      const sc = innerWidth < 1000 ? tc : null, to = top => sc ? (sc.scrollTop = top) : scrollTo({ top, behavior: 'instant' });
      to(1e6); await wait();
      to(0); // headless paints no frames, so it fires no scroll event: send the scroller's own
      const y = a.getBoundingClientRect().top;
      (sc || window).dispatchEvent(new Event('scroll'));
      await wait();
      const n = tc.querySelectorAll(':scope>[data-n]').length, d = a.getBoundingClientRect().top - y;
      if (n < 40 || Math.abs(d) > 1 || tc.querySelector('.tmo')) early.push(`older page: ${n} messages, the one on screen moved ${d} px`);
      // A view scrolled past about one screen shoots blank (chat-thread): the shot keeps the page's last 3 at the top.
      if (!DBG.get('check')) { [...tc.querySelectorAll(':scope>[data-n]')].slice(0, 27).forEach(e => { e.hidden = true; }); scrollTo({ top: 0, behavior: 'instant' }); tc.scrollTop = 0; }
    },
    'chat-mid': async () => {
      await VIEWS['chat-older']();
      const tc = $('#chat .cvp[data-t="t4"] .tc');
      if (innerWidth < 1000) tc.scrollTop = tc.scrollHeight / 2; else scrollTo({ top: document.documentElement.scrollHeight / 2, behavior: 'instant' });
      await wait();
      const th = $('#chat .cvp[data-t="t4"] .th'), r = th.getBoundingClientRect(), hb = $('header').getBoundingClientRect().bottom;
      if (Math.abs(r.top - hb) > 1 || !th.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2))) early.push(`thread header at ${r.top} px, not pinned under the page header (${hb})`);
      // The shot can't scroll (chat-older): the banner and the thread above its middle go, the header over the rest.
      if (!DBG.get('check')) { $('.msg.a').hidden = true; $$('#chat .cvp[data-t="t4"] .tc>*').forEach((e, i) => { e.hidden = i < 30; }); scrollTo({ top: 0, behavior: 'instant' }); tc.scrollTop = 0; }
    },
    // #405 gate 2: [+ thread]'s empty thread (`new-<repo>`, as the tap opens it): no title form, no blocks, the
    // composer (its first message starts the thread); the demo's controls inert, so enabled as a paired phone's.
    'chat-new': () => {
      tab('chat'); chatOpen(''); $('.lall')?.click();
      chatOpen('new-acme-api');
      const v = $('#chat .cvp[data-t="new-acme-api"]');
      v.querySelectorAll('[disabled]').forEach(e => { e.disabled = false; });
      if (!seen(v) || v.querySelector('.tc').childElementCount || v.querySelector('input:not([type=file])') || v.querySelector('.tw').dataset.repo !== 'acme-api') early.push('the new thread is not empty with a composer for acme-api');
    },
    // #405 gate 2: a thread row swiped left shows [archive]; swiped right it hides; a short or downward drag does nothing.
    'chat-archive': () => {
      tab('chat'); chatOpen(''); $('.lall')?.click();
      const r = $('#chat .tr[data-t=t4]'), b = r.parentNode.querySelector('.sb[name=archive]'), y = r.getBoundingClientRect().top + 10;
      const swipe = (x0, x1, dy = 0) => {
        r.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: x0, clientY: y }));
        r.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, clientX: x1, clientY: y + dy }));
      };
      b.disabled = false;
      for (const [what, x0, x1, dy, want] of [['a short drag', 300, 280, 0, false], ['a downward drag', 300, 240, 200, false], ['a left swipe', 300, 150, 0, true], ['a right swipe', 150, 300, 0, false], ['a left swipe again', 300, 150, 0, true]]) {
        swipe(x0, x1, dy);
        if (seen(b) !== want) early.push(`${what}: [archive] ${seen(b) ? 'shows' : 'hidden'}`);
      }
      scrollTo({ top: 0, behavior: 'instant' });
    },
    // #405 gate 2: the archived fold at the bottom of its repo card, open: t1 with [restore].
    'chat-archived': () => {
      tab('chat'); chatOpen(''); $('.lall')?.click();
      const f = $('#chat .fc .arc');
      if (!f) return early.push('no archived fold');
      f.open = true;
      f.querySelectorAll('[disabled]').forEach(e => { e.disabled = false; });
      if (f.nextElementSibling || !f.querySelector('.tr[data-t=t1]') || !f.querySelector('.sb[name=restore]') || $('#chat .fc>.sw .tr[data-t=t1]')) early.push('t1 is not in its card\'s last fold with [restore]');
    },
    // #405 gate 2: three images picked at once wait above t3's box as thumbnails, each with [×]; [×] drops one.
    'chat-thumbs': async () => {
      const f = composer(), i = f.querySelector('input[type=file]'), d = new DataTransfer();
      const srcs = [...$$('#chat .cvp[data-t="t4"] .im img')].map(m => m.src);
      for (const [n, src] of [srcs[0], srcs[1], srcs[0]].entries()) d.items.add(new File([await fetch(src).then(r => r.blob())], `IMG_20${41 + n}.PNG`, { type: 'image/png' }));
      i.files = d.files;
      i.dispatchEvent(new Event('change', { bubbles: true }));
      const th = () => [...$$('#chat .tw[data-t=t3] .att')].filter(a => a.querySelector('img') && a.querySelector('button'));
      if (th().length !== 3) early.push(`${th().length} thumbnails with [×], not 3`);
      if (DBG.get('check') && th()[1]) { th()[1].querySelector('button').click(); if (th().length !== 2) early.push(`[×]: ${th().length} left, not 2`); }
      else $$('#chat .cvp[data-t=t3] .tc>*').forEach(e => { e.hidden = true; });
    },
    // #397: a start error on the [chat] tab, as `thread say` failed on the mini.
    'chat-error': () => { tab('chat'); chatOpen(''); $('.lall')?.click(); scrollTo({ top: 0, behavior: 'instant' }); toast(ERR); },
    // #276: long `now:` lines as the server emits them (one-line classes, full text in title): plain, Claude's spinner word
    // + a long action + its time, and an idle row with no spinner, drawn by a /live redraw of the Lanes tab, then a
    // second one: one ellipsised line in each mode, nothing past the row.
    'lanes-now': async () => {
      tab('lanes');
      const sp = (...p) => p.map((x, i) => `<span class=${i < 2 ? 'el' : 't'}>${esc(x)}</span>`).join('<i> · </i>');
      const L = [
        sp('now: running git fetch origin && git merge origin/staging 2>&1 | tail -5; just --list; cat ci/cap.toml'),
        sp('✻ Simmering…', 'running git merge origin/staging +2 cd ~/w/issue-2684; S=/tmp/s; FILES=$(git diff --name-only | rg -v x)', '4m'),
        sp('✻ Herding…', 'editing src/a/very/long/path/to/some/deeply/nested/module/that/runs/past/the/edge.rs'),
        sp('now: running cd ~/w/issue-2684; S=/tmp/s; FILES=$(git diff --name-only | rg -v \'^docs/\'); cargo fmt --check $FILES')];
      const redraw = n => {
        const c = $('#lanes').cloneNode(true);
        [...c.querySelectorAll('.lane .act')].slice(0, 5).forEach((a, i) => {
          a.outerHTML = i === 4 ? '<span class=act></span>' : `<span class="act nw el now" title="full">${L[(i + n) % 4]}</span>`;
        });
        frag('lanes', c.innerHTML);
      };
      const one = what => $$('.lane .act.now').forEach(a => {
        // Lines by box height (a Range's rects mix the span boxes and their text runs, whose tops differ on one line).
        const r = a.getBoundingClientRect(), lines = Math.round(r.height / parseFloat(getComputedStyle(a).lineHeight));
        const cut = [...a.querySelectorAll('.el')].some(e => getComputedStyle(e).textOverflow !== 'ellipsis' || e.getBoundingClientRect().right > r.right + 1);
        if (lines !== 1 || cut || r.right > a.closest('.lane').getBoundingClientRect().right + 1) early.push(`${what}: now-line "${a.textContent.slice(0, 30)}…" ${lines} lines, right ${r.right}`);
        const [w, x] = a.querySelectorAll('.el');
        if (x && w.scrollWidth > w.clientWidth + 1 && x.clientWidth > 0) early.push(`${what}: the word "${w.textContent}" cut before the action`);
      });
      for (const n of [0, 1]) {
        redraw(n);
        for (const m of TM) { mode(m); await wait(); one(`after /live ${n + 1}, ${m}`); }
      }
      if ($$('.lane .act.now').length < 4) early.push('no long now-lines drawn');
    },
    sheet: async () => { // the longest agent name (not a conductor row, #422: it opens a thread), then kill refused, kill's dry run, clean refused, a send error
      tab('lanes');
      open([...$$('.lane[data-agent]:not([data-thread])')].sort((a, b) => b.dataset.agent.length - a.dataset.agent.length)[0]);
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
    // #260: the same sheet with [full conversation] on (every tool run); `sheet` itself is the default view.
    'sheet-full': async () => {
      tab('lanes');
      open([...$$('.lane[data-agent]:not([data-thread])')].sort((a, b) => b.dataset.agent.length - a.dataset.agent.length)[0]);
      await wait();
      $('.fv').click();
      await wait();
      if ($('.fv').ariaPressed !== 'true' || !$('.chat>.now .blk.run')) early.push('[full conversation]: not pressed, or no tool run shown');
    },
    // #750: a lane whose transcript is an ACP session's (shed's `> prompt`, `[tool] title (status)`, the reply, as
    // crates/cli/tests/golden/screens/acp.txt): its text on separate lines, each tool one line, its rest behind a tap.
    'sheet-acp': async () => {
      const lines = ['> # Brief — acme-web-impl-41 (acme/acme-web#41)', 'Implement #41: dark-mode toggle persists across reloads.', 'Run `pnpm test` before you push.',
        '[tool] Read src/settings/store.ts (completed)', '[tool] Edit src/settings/store.ts (completed)',
        '[tool] `cd /srv/acme/worktrees/acme-web-impl-41 && pnpm test settings --reporter=dot 2>&1 | tee test.log`', '`git add -A && git commit -m "settings: keep the theme across reloads (#41)"` (completed)',
        '[tool] git push -u origin issue-41 (failed)', 'The theme now loads from storage before the first paint.', 'Tests pass; the push was refused, so I stopped there.'];
      window.fetch = (u, o) => String(u).includes('lane/') ? Promise.resolve(Response.json({ harness: 'claude', lines, live: 0 })) : F(u, o);
      tab('lanes');
      open($('.lane[data-agent="acme-web-impl-41"]'));
      await wait();
      const txt = [...$$('.chat>.now>.txt')], tools = [...$$('.chat>.now>.blk.run')];
      if (txt.length !== 2 || getComputedStyle(txt[0]).whiteSpace !== 'pre-wrap' || txt[0].offsetHeight < 3 * parseFloat(getComputedStyle(txt[0]).lineHeight)) early.push(`text: ${txt.length} blocks, not 2 on separate lines`);
      if (tools.length !== 4 || tools.some(t => t.open || t.querySelector('.cmd').offsetHeight > 1.5 * parseFloat(getComputedStyle(t.querySelector('.cmd')).lineHeight))) early.push(`tools: ${tools.length}, not 4 closed one-line rows`);
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
      if ($('.chat>.now').textContent !== 'no pane: the transcript closed with it') early.push(`chat reads "${$('.chat>.now').textContent.slice(0, 40)}"`);
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
      if (st.textContent !== 'not a drover lane: drover can only close its pane') early.push(`p.st reads "${st.textContent}"`);
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
      if (!shown() || !S.matches('.shut') || aim() < sh.offsetHeight - 1 || slide !== '0.2s') dragged.push(`a frame after close: shown ${shown()}, sliding to ${aim() | 0}px of ${sh.offsetHeight} over ${slide}: no slide`);
      await wait();
      if (shown()) dragged.push('300 ms after close the sheet is still shown');
      if (!DBG.get('check')) { open($('.lane')); S.classList.add('drag'); snap('half'); } // the screenshot: the sheet at half, unanimated
    },
    host: async () => { // Hosts & health; #440: then Notifications' live [enable alerts] (the export leaves it out) as the Home Screen app shows it: an error, then unpaired
      sys('hosts');
      ranked();
      const push = Object.assign(document.createElement('button'), { id: 'push', type: 'button', textContent: 'Enable alerts' });
      push.dataset.key = 'AA';
      const al = [...$$('#sy-notifications dt')].find(t => t.textContent === 'this phone').nextElementSibling;
      al.prepend(push);
      tidy(al);
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
      sysopen('hosts'); // drawn() checks Hosts & health; the measure takes all of System, Notifications' [enable alerts] too
    },
    // #234 gate 2: the hosts fixture with mini's load/cpu chart open (24 h), read from the export's inline series.
    'host-chart': async () => {
      sys('hosts');
      const r = $('#host .hcd .rw[data-k=load]');
      r.closest('.grp').open = true;
      r.open = true;
      await chart(r, '24h', null, true);
      if (!r.querySelector('.hpl polyline.ln') || !$$('#host .spk polyline').length) early.push('host chart: no sparkline or chart line drawn');
      if (!DBG.get('check')) r.closest('.hcd').scrollIntoView();
    },
    // #141 gate 2: the busy and healthy fixtures (scripts/host-fixtures.sh), as the tab first shows them.
    'host-ranked': () => {
      sys('hosts');
      ranked();
      if (location.pathname.includes('/healthy/') && $$('#host>.grp:is([open],:not([data-t=g]))').length) early.push('healthy: a group is open or not green');
    },
    // #154: three needs-you cards through the live path (frag): [later] on one → `1 need you ▸` and 2 cards; [later all]
    // → the line alone; a tap on it → all 3; a changed card (its question) comes back by itself. Measured as it ends.
    'needs-later': () => {
      tab('lanes');
      put('later', '{}');
      const cards = [...$$('#needs>.ask')].slice(0, 3).map(c => { const x = c.cloneNode(true); x.querySelector('.lat').remove(); x.className = 'ask'; return x; });
      if (cards.length < 3) return early.push(`${cards.length} needs-you cards in the demo, not 3`);
      frag('needs', cards.map(c => c.outerHTML).join(''));
      const line = () => $('#needs').previousElementSibling, shown = () => [...$$('#needs>.ask')].filter(seen).length;
      const want = (what, text, n) => { const t = seen(line()) ? line().textContent : 'no line'; if (t !== text || shown() !== n) early.push(`${what}: line "${t}", ${shown()} cards, not "${text}" and ${n}`); };
      want('fresh', 'no line', 3);
      $('#needs .lat').click();
      want('[later] on one', '1 need you ▸later all', 2);
      $('.later .lall').click();
      want('[later all]', '3 need you ▸later all', 0);
      $('.later .lo').click();
      want('tap the line', '3 need you ▾later all', 3);
      $('.later .lo').click();
      cards[1].querySelectorAll(':scope>p')[1].textContent += ' (changed)';
      frag('needs', cards.map(c => c.outerHTML).join(''));
      want('a changed card', '2 need you ▸later all', 1);
      if (!seen($$('#needs>.ask')[1])) early.push('the changed card did not come back');
    },
    // #217 gate 2: a tapped [one more round]: the needs-human card is one muted pending line with no buttons, at the top.
    // #859: an ask's answer tap takes its card off at once, before the POST answers; a failed POST draws the cards again.
    'needs-answered': async () => {
      tab('lanes');
      $('#needs').insertAdjacentHTML('afterbegin', '<div class=ask id=aud859><p>Which DB?</p><form><input type=hidden name=op value=answer><input type=hidden name=fleet value=acme-web><input type=hidden name=id value=q859><button name=choice value=1>1. sqlite</button></form></div>');
      const hold = [];
      window.fetch = (u, o) => o && o.method === 'POST' ? new Promise(r => hold.push(r)) : F(u, o);
      $('#aud859 form').requestSubmit($('#aud859 button'));
      if (!hold.length || $('#aud859')) early.push(`after the tap: ${hold.length} POSTs, the card ${$('#aud859') ? 'still shown' : 'gone'}`);
      window.fetch = F;
    },
    'needs-pending': () => {
      tab('lanes');
      const c = [...$$('#needs>.ask[data-n]')].find(c => /needs you/.test(c.textContent));
      if (!c) return early.push('no needs-human card in the demo');
      $('#needs').prepend(c);
      pend(c, 'extend');
      const p = $('#needs>.pend');
      if (!p || p.querySelector('button,form') || p.textContent !== '⟳ one more round · starting a lane…') early.push(`pending card: ${p ? p.outerHTML : 'none'}`);
    },
    // #170 gate 2: [important] in all 5 time groups, a red glyph, its first row open; no fold holds clean and a failure.
    events: () => {
      sys('activity');
      const imp = $('#events [data-v=imp]'), gs = [...imp.querySelectorAll('.eg>h3>span:first-child')].map(s => s.textContent);
      if (gs.join() !== 'now,5m,1h,earlier today,yesterday') early.push(`[important] groups: ${gs.join()}`);
      if (!imp.querySelector('.er .kg.t-r')) early.push('no red glyph under [important]');
      if ([...$$('#events .er')].some(r => /clean/.test(r.textContent) && /alert failed/.test(r.textContent))) early.push('a row holds clean and a failure');
      imp.querySelector('.er').open = true;
    },
    // #241: a pick applies in place: no reload, the favicon swaps, terminal strips every sprite, kitchen draws its own.
    'theme-switch': async () => {
      tab('queue');
      window.kept = 1;
      sys('appearance'); // #290: the look sheet's rows, not a select; #440: System's Appearance
      const pick = n => { $(`.look .lko[data-t="${n}"]`).click(); return new Promise(r => setTimeout(r, 800)); };
      await pick('terminal');
      if (window.kept !== 1 || document.documentElement.dataset.theme !== 'terminal' || get('theme') !== 'terminal') early.push('the switch to terminal reloaded the page or was not kept');
      if (!$('link[rel=icon]').href.includes('themes/terminal/icon-')) early.push(`favicon ${$('link[rel=icon]').href}, not terminal's`);
      if ($$('.rsp, .lsc').length || B.dataset.mode !== 'glyph') early.push('terminal kept sprites or a scene mode');
      await pick('kitchen');
      if (!$$('#queue .rsp').length || !$('#hr-impl .chef, #hr-impl [shape-rendering]')) early.push("kitchen: no row sprites, or not the chef's");
      sysshut(); // the board is measured, not System over it
    },
    // #290 gate 2, #440: [⚙] on all 3 tabs (no [◐], no native select) opens System at Agent providers; its Appearance
    // holds THEME and COLOURS listboxes, one row ticked each; a tap applies in place (no reload) and the sheet stays open.
    look: async () => {
      for (const t of ['lanes', 'chat', 'queue']) { tab(t); if (!seen($('#sysb')) || $('#look')) early.push(`no [⚙], or a [◐], on ${t}`); }
      if ($$('select#theme, select#scheme').length) early.push('a native theme/colours select remains');
      $('#sysb').click();
      if (!sysOn() || location.hash !== '#system/providers' || ytop.scrollTop !== 0 || !$('.sxi a.on[data-s=providers]')) early.push(`[⚙] opened ${location.hash}, not System at providers`);
      sysopen('appearance');
      const ls = [...$$('#sy-appearance [role=listbox]')].map(l => `${l.getAttribute('aria-label')}:${l.querySelectorAll('[role=option]').length}/${l.querySelectorAll('[aria-selected=true]').length}`);
      if (ls.length !== 3 || !/^theme:4\/1,colours:\d+\/1,style:2\/1$/.test(ls.join())) early.push(`look sheet lists: ${ls.join()}`);
      // #335: STYLE ascii applies in place (rows take the figures, the sheet stays open), pixel puts the sprites back.
      $('.look .lko[data-y=ascii]').click();
      if (B.dataset.mode !== 'ascii' || get('mode') !== 'ascii' || !$$('#queue .rsa').length || $$('#queue .rsg>svg').length || !sysOn()) early.push('style ascii: not applied in place');
      $('.look .lko[data-y=pixel]').click();
      if (B.dataset.mode !== 'scene' || $$('.rsa, .lsa').length) early.push('style pixel: ascii figures remain');
      window.kept = 1;
      const s = [...$$('.look .lko[data-s]')].find(o => o.getAttribute('aria-selected') !== 'true');
      s.click();
      if (window.kept !== 1 || document.documentElement.dataset.scheme !== s.dataset.s || !sysOn()) early.push('a colours tap reloaded, missed or closed the sheet');
      sysshut(); // closed, [⚙] opens it again
      $('#sysb').click();
      if (!sysOn()) early.push('[⚙] did not reopen System after a close');
      sysopen('appearance');
    },
    // #370 gate 2: a row sprite loops its two frames (--o1 flips within 0.7 s) and is the same node across a /live
    // patch between the two reads; `row-loop-still` (reduced motion forced): frame 1, no animation, still the same node.
    'row-loop': async () => {
      tab('queue');
      $('#needs').hidden = true;
      const q = () => [...$$('#queue .gt>b.rsg>.rsp')].find(seen), u = q(), still = V === 'row-loop-still', o = () => getComputedStyle(u.querySelector('use')).getPropertyValue('--o1').trim();
      if (!u) return early.push('no Queue row sprite');
      if (still && !matchMedia('(prefers-reduced-motion: reduce)').matches) early.push('reduced motion not forced');
      // The loop's clock is the page's animation (the Chrome run's virtual time never ticks CSS): frame 1 at 0 ms,
      // frame 2 at 0.7 s, set on the animation itself, across a /live patch.
      const at = ms => { const k = u.getAnimations().find(x => x.animationName === 'kfr'); if (k) k.currentTime = ms; return k ? o() : 'none'; };
      const f1 = at(0);
      frag('queue', $('#queue').innerHTML);
      const f2 = at(700);
      if (q() !== u || !u.isConnected) early.push('the row sprite was replaced by a /live patch');
      if (still ? f1 !== 'none' || getComputedStyle(u).animationName !== 'none' || o() !== '1' && o() !== '' : f1 !== '1' || f2 !== '0') early.push(`row sprite --o1 ${f1} at 0 ms, ${f2} at 0.7 s (${getComputedStyle(u).animationName})`);
    },
    // #370 gate 2: a moment plays on its row once per event: a second call with the same seq does nothing, a /live patch
    // mid-moment resumes it (a negative delay, not a restart), it ends by 2.4 s; `moment-row-still`: no motion, its tag shows.
    'moment-row': async () => {
      tab('queue');
      $('#needs').hidden = true;
      // three rows, one moment each, played side by side (the Chrome run's virtual time is 8 s)
      const ds = [...$$('#queue details.q')].filter(seen).slice(0, 3);
      if (ds.length < 3) return early.push('under 3 Queue rows in view');
      let patched; const once = new Promise(r => patched = r); // the other rows read after the one patch, never a clock
      await Promise.all(['fail', 'needs-you', 'merged'].map(async (k, i) => {
        const n = +ds[i].querySelector('summary a').textContent.slice(1), f = qname(ds[i].closest('.fc')), seq = 1e9 + i;
        const sum = () => [...$$('#queue details.q')].find(x => x.querySelector('summary a').textContent === '#' + n && x.closest('.fc')?.querySelector('.fn>b').textContent === f).querySelector('summary');
        rmoment(k, f, n, seq);
        const t = sum().querySelector('.mot');
        if (sum().dataset.mo !== k || !t || t.textContent.replace('!', '') !== TW()[MK[k]]) early.push(`${k}: not on its row, or no "${TW()[MK[k]]}" tag`);
        await new Promise(r => setTimeout(r, 1100));
        rmoment(k, f, n, seq);
        if (i === 2) { // the server's copy: no moment marks
          const x = $('#queue').cloneNode(true);
          x.querySelectorAll('[data-mo]').forEach(e => { delete e.dataset.mo; e.style.removeProperty('--mo-d'); e.querySelector('.mot')?.remove(); });
          frag('queue', x.innerHTML);
          patched();
        }
        else await once;
        const dl = parseFloat(sum().style.getPropertyValue('--mo-d'));
        if (sum().dataset.mo !== k || !(dl <= -1000)) early.push(`${k}: a patch at 1.1 s restarted or dropped it (--mo-d ${dl})`);
        if (V === 'moment-row-still' && [sum(), sum().querySelector('.mot')].some(e => !e || getComputedStyle(e).animationName !== 'none')) early.push(`${k}: moves under reduced motion`);
        await new Promise(r => setTimeout(r, 1500));
        if (sum().dataset.mo || sum().querySelector('.mot')) early.push(`${k}: still on at 2.6 s`);
      }));
    },
    // #370: empty tabs: the theme's line over the server's plain one (terminal: the plain line alone).
    empty: () => {
      frag('lanes', '<p class=m>no agents</p>');
      frag('events', '<div data-v=imp><p class="m es">0 important rows</p></div>');
      frag('queue', '<p class=m>no open work</p>');
      tab('queue');
      for (const [t, k] of [['queue', 'empty-queue'], ['lanes', 'empty-lanes'], ['events', 'empty-events']]) {
        const p = $(`#${t} p.m:not(.es)`), w = TW()[k];
        if (!p || (w ? p.querySelector('.thw')?.textContent !== w || !p.querySelector('.pw') : p.innerHTML !== p.textContent)) early.push(`${t}: empty line "${p && p.innerHTML}"`);
      }
    },
    // #241: a moment under reduced motion is a still frame (scripts/ui-audit.sh runs this view with it forced).
    'moment-still': () => {
      tab('queue');
      moment('merged');
      const o = $('#mo');
      if (!matchMedia('(prefers-reduced-motion: reduce)').matches) early.push('reduced motion not forced');
      if (!o || o.hidden || getComputedStyle(o).animationName !== 'none') early.push(`moment: ${o ? getComputedStyle(o).animationName : 'none shown'}, not a still frame`);
    },
    // #450 gate 2: a wait in the phone's theme (`loading~kitchen`, `loading~terminal`; `loading-still` with reduced motion
    // forced; `-1440` at the desktop width): no overlay. A load through wait() under 300 ms shows nothing; a slower one the
    // strip under the header, one thin row: the chat's inline mark (the theme's sprite at 24 px; terminal: a spinning
    // glyph; reduced motion: one still frame) + `loading`, gone when the load ends; [↻] keeps its box and spins its plain ↻.
    // The shot is a load that never ends.
    loading: async () => {
      tab('lanes');
      const box = () => [rf, rf.firstChild, $('header')].map(e => { const r = e.getBoundingClientRect(); return [r.left, r.top, r.width, r.height].map(Math.round).join(); }).join(' ');
      const idle = box(), still = matchMedia('(prefers-reduced-motion: reduce)').matches, ov = $('#ld'), svg = ov.querySelector('svg'), g = ov.querySelector('i'), sp = $('#strip');
      const ms = n => new Promise(r => setTimeout(r, n));
      if (rf.matches('.spin') || seen(ov) || sp.offsetHeight) early.push(`busy before the load (strip ${sp.offsetHeight} px)`);
      if ($('header svg.lds, header use[href="#hload"]')) early.push('a loading sprite in the header');
      let fast = 0;
      const mo = new MutationObserver(() => { fast += seen(ov); });
      mo.observe(ov, { attributes: true });
      await window.wait(ms(100));
      await ms(300);
      mo.disconnect();
      if (fast) early.push('a 100 ms load showed the strip');
      const half = window.wait(ms(600));
      await ms(400);
      if (!seen(ov)) early.push('a 600 ms load: no strip at 400 ms');
      await half;
      if (seen(ov) || sp.offsetHeight) early.push(`the strip stayed after the load (${sp.offsetHeight} px)`);
      busy = true;
      sync('a');
      window.wait(new Promise(() => {}));
      await ms(400);
      if (box() !== idle) early.push(`[↻] busy moved: ${idle} → ${box()}`);
      if (V === 'loading-still' && !still) early.push('reduced motion not forced');
      if (rf.querySelector('svg') || rf.textContent !== '↻') early.push('[↻] is not the plain ↻');
      const spin = getComputedStyle(rf.firstChild).animationName;
      if (spin !== (still ? 'none' : 's')) early.push(`[↻] ${spin}, not spinning`);
      const hb = $('header').getBoundingClientRect(), sr = sp.getBoundingClientRect(), t = document.documentElement.dataset.theme, term = t === 'terminal';
      if (Math.abs(sr.top - hb.bottom) > 1 || sr.height > 36) early.push(`the strip: ${sr.top - hb.bottom | 0} px under the header, ${sr.height} px tall`);
      if ($('#strip .ldw').textContent !== 'loading' || !seen($('#strip .ldw'))) early.push('the strip does not say loading');
      if ([...$$('body *')].some(e => getComputedStyle(e).position === 'fixed' && seen(e) && e.getBoundingClientRect().height > innerHeight / 2 && e.contains(ov))) early.push('a full-screen loading overlay');
      if (term ? seen(svg) || !seen(g) : !ov.matches('.ld') || !$('#hload') || !seen(svg) || seen(g)) early.push(`${t}: not its loading mark (#hload ${!!$('#hload')})`);
      const el = term ? g : svg, r = el.getBoundingClientRect(), cs = getComputedStyle(el);
      if (!term && (Math.round(r.width) !== 24 || Math.round(r.height) !== 24)) early.push(`${t}: loading sprite ${r.width}×${r.height}, not the chat's 24×24`);
      if (cs.animationName !== (still ? 'none' : term ? 's' : 'ldg, ldk')) early.push(`${t}: loading mark ${cs.animationName}`);
    },
    // #450: during a load that never ends, a tap on a lane (whatever is at its point on screen) opens its sheet.
    'loading-tap': async () => {
      tab('lanes');
      window.wait(new Promise(() => {}));
      await new Promise(r => setTimeout(r, 400));
      const l = $('#lanes .lane:not([data-thread]) .lr');
      l.scrollIntoView({ block: 'center' });
      const r = l.getBoundingClientRect(), hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      if (!seen($('#ld'))) early.push('no strip while loading');
      if (!l.contains(hit)) early.push(`the lane is covered by ${hit && hit.outerHTML.slice(0, 60)}`);
      else hit.click();
      if (!shown()) early.push('a tap during the load opened no sheet');
    },
    // #450: data 3 min old: `as of 3m ago` in the strip, small and dim; with a load as well, both on its one row.
    stale: () => {
      tab('lanes');
      last = Date.now() - 180000;
      age();
      const a = $('#stale'), hb = $('header').getBoundingClientRect(), sr = $('#strip').getBoundingClientRect();
      if (!seen(a) || a.textContent !== 'as of 3m ago') early.push(`the age: "${a.textContent}" ${seen(a) ? 'shown' : 'hidden'}`);
      if (Math.abs(sr.top - hb.bottom) > 1 || sr.height > 36) early.push(`the strip: ${sr.top - hb.bottom | 0} px under the header, ${sr.height} px tall`);
      if (getComputedStyle(a).fontSize !== getComputedStyle($('.tlv') || a).fontSize) early.push(`the age at ${getComputedStyle(a).fontSize}`);
      loading(true);
      if (Math.round(a.getBoundingClientRect().top) !== Math.round($('#strip .ldw').getBoundingClientRect().top) || $('#strip').getBoundingClientRect().height > 36) early.push('loading and the age are not on one row');
      last = Date.now() - 60000;
      age();
      if (seen(a)) early.push('a copy 1 min old shows its age');
      last = Date.now() - 180000;
      age();
    },
    // #450: a first load with nothing cached: the strip and the tab's empty outlines, no text, no chef.
    'first-load': () => {
      $('#lanes').replaceChildren();
      tab('lanes');
      loading(true);
      const o = getComputedStyle($('#lanes'), '::before');
      if (o.content === 'none' || parseFloat(o.height) < 40 || o.borderTopStyle === 'none') early.push(`no outline: ${o.content} ${o.height} ${o.borderTopStyle}`);
      if ($('#lanes').textContent) early.push('text in the outlines');
      if (!seen($('#ld'))) early.push('no strip');
    },
    'loading-still': () => VIEWS.loading(),
    // #335 gate 2: ascii style (the look sheet's STYLE): the rows, Lanes, Host and the wait in the theme's ASCII figures.
    'ascii-queue': () => { style('ascii'); return VIEWS.queue(); },
    'ascii-lanes': () => { style('ascii'); return VIEWS.lanes(); },
    'ascii-host': () => { style('ascii'); return VIEWS.host(); },
    'ascii-events': () => { style('ascii'); return VIEWS.events(); },
    // The strip's mark runs its frames by CSS alone: no node or text changes while it shows (#334), [↻] stays the plain ↻.
    'ascii-loading': () => {
      style('ascii');
      tab('queue');
      const ov = $('#ld'), a = ov.querySelector('.lda');
      let n = 0;
      const mo = new MutationObserver(m => { n += m.length; });
      busy = true;
      sync('a');
      loading(true);
      mo.observe(ov, { subtree: true, childList: true, characterData: true, attributes: true });
      if (!a || !seen(a) || seen(ov.querySelector('svg')) || seen(ov.querySelector('i'))) early.push('ascii: not the ascii loading frames');
      if (a && (a.children.length !== 2 || getComputedStyle(a.lastChild).animationName !== 'f')) early.push(`ascii: ${a.children.length} loading frames, ${getComputedStyle(a.lastChild).animationName}`);
      if (rf.querySelector('svg') || rf.textContent !== '↻') early.push('[↻] is not the plain ↻');
      setTimeout(() => { mo.disconnect(); if (n) early.push(`${n} changes to the loading mark while it ran`); }, 1100);
    },
    herd1: () => { herd(true); light = 'day'; ranch(); }, // #71: day shots, whatever the runner's clock says
    herd2: () => { herd(true); light = 'day'; ranch(); },
    // #172: the kitchen theme (`?theme=kitchen`), and it missing `done` (`&nopose=done`): farm's done horse.
    // #239: its stations and chefs by role at calm, busy (the demo's [gusty]) and held ([storm]: smoke, wall flames, the red glow,
    // idle chefs fanning); `herd-kitchen-still` is held under reduced motion: a still frame that still shows smoke and glow.
    'herd-kitchen': () => kitchen('calm'),
    'herd-kitchen-busy': () => kitchen('gusty'),
    'herd-kitchen-held': () => kitchen('storm'),
    'herd-kitchen-still': () => kitchen('storm'),
    // #423: the garden (`?theme=garden`): the shop window's sun, cloud and rain by load, each lane the keeper over its
    // issue's plant; `herd-garden-still` holds one frame under reduced motion.
    'herd-garden': () => garden('calm'),
    'herd-garden-busy': () => garden('gusty'),
    'herd-garden-held': () => garden('storm'),
    'herd-garden-still': () => garden('storm'),
    'herd-nodone': () => { herd(true); light = 'day'; ranch(); setTimeout(() => { if (!$('#hp3 .horse-done') || $('#hp1 .horse')) early.push("nodone: done is not farm's horse, or the rest not chefs"); }, 500); },
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
    'herd-dupes': () => herd(true),
    // #291 gate 2: 3 repos; [›], a dot, →, a horizontal wheel (one gesture of three events), shift+wheel and a mouse drag
    // across a horse each move exactly one repo; a vertical wheel and a drag in a text field none; the drag's click opens nothing, a plain click does.
    'herd-nav': async () => {
      herd(true); light = 'day'; ranch();
      await new Promise(r => setTimeout(r, 500));
      const f = structuredClone(data);
      f.fleets = f.fleets.slice(0, 3); f.others = [];
      herdData(f);
      const wh = o => PG.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, ...o }));
      const pe = (t, el, x) => el.dispatchEvent(new PointerEvent(t, { bubbles: true, cancelable: true, pointerId: 7, pointerType: 'mouse', button: 0, buttons: t === 'pointerup' ? 0 : 1, clientX: x, clientY: 400 }));
      const steps = [['[›]', 1, () => hp(0).querySelector('.hnx').click()], ['the next dot', 1, () => hp(0).querySelectorAll('.hdt button')[1].click()],
        ['→', 1, () => B.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }))],
        ['a horizontal wheel', 1, () => [50, 50, 50].forEach(x => wh({ deltaX: x }))], ['a vertical wheel', 0, () => wh({ deltaY: 120 })],
        ['shift+wheel', 1, () => wh({ deltaY: 100, shiftKey: true })],
        ['a drag in a text field', 0, () => { const i = hp(0).appendChild(document.createElement('input')); pe('pointerdown', i, 300); [280, 240, 200].forEach(x => pe('pointermove', i, x)); pe('pointerup', i, 200); i.remove(); }],
        ['a mouse drag', 1, () => { const h = H.querySelector('.hh'); pe('pointerdown', h, 300); [280, 240, 200].forEach(x => pe('pointermove', h, x)); pe('pointerup', h, 200); h.click(); }]];
      // Reduced motion (the gate run): each switch lands at once; else after the smooth scroll. A step waits out a wheel gesture.
      for (const [what, want, run] of steps) {
        PG.scrollTo({ left: 0, behavior: 'instant' });
        await new Promise(r => setTimeout(r, 350));
        run();
        if (!still.matches) await new Promise(r => setTimeout(r, 900));
        const got = Math.round(PG.scrollLeft / PG.clientWidth);
        if (got !== want) early.push(`${what}: on repo ${got}, not ${want}${still.matches ? ' at once (reduced motion)' : ''}`);
      }
      if (shown()) early.push('the click ending a drag opened a lane');
      if (getSelection().toString()) early.push(`the drag selected "${getSelection()}"`);
      H.querySelector('.hh').click();
      if (!shown()) early.push('a plain click on a horse opened nothing');
      close();
      hide();
      $$('.hnv, .hdt button').forEach(b => { if (getComputedStyle(b).cursor !== 'pointer') early.push(`${label(b)}: cursor ${getComputedStyle(b).cursor}`); });
    },
    // #367 gate 2 (the `queue` fixture: 3 repos, 40 queued, a 6-step chain): a tap, ←/→ and a sideways drag switch repos
    // (only the picked card shows, the pick stored); back on acme-core its running rows are above the fold, the chain one
    // closed row with its summary, `+ 30 more` closed, and every title line ≥ 4.5:1 on what is under it.
    'queue-repos': () => {
      tab('queue');
      const vis = () => [...$$('#queue .fc')].filter(seen).map(qname).join(), chip = n => $(`.qch [data-qr="${n}"]`);
      const tries = [['tap', () => chip('docs').click(), 'docs'], ['→', () => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true })), 'all'],
        ['←', () => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true })), 'docs'],
        ['swipe', () => { const t = $('#queue .fc:not([hidden]) .qg'), o = { bubbles: true, pointerId: 7, clientY: 300 }; t.dispatchEvent(new PointerEvent('pointerdown', { ...o, clientX: 300 })); t.dispatchEvent(new PointerEvent('pointerup', { ...o, clientX: 150 })); t.click(); }, 'all']]; // a mouse drag ends in a click, which opens nothing
      for (const [how, act, want] of tries) {
        act();
        const shows = want === 'all' ? [...$$('#queue .fc')].filter(c => c.querySelector('.qg[data-g=need]')).map(qname).join() : want;
        if (vis() !== shows || get('queue:repo') !== want || chip(want).getAttribute('aria-pressed') !== 'true') early.push(`${how}: shows "${vis()}", stored ${get('queue:repo')}, not ${want}`);
      }
      if ([...$$('.fc.qa .qg')].some(g => seen(g) && g.dataset.g !== 'need')) early.push('[all] shows a group that is not needs you');
      chip('acme-core').click();
      const nav = $('nav').getBoundingClientRect().top, prog = [...$$('.qg[data-g=prog]>details.q')].filter(seen);
      if (!prog.length || prog.some(d => d.getBoundingClientRect().bottom > nav)) early.push(`running rows not above the fold (tab bar at ${nav})`);
      const gs = [...$$('#queue .qg>h3')].filter(seen).map(h => h.dataset.p ?? h.textContent).join(', ');
      if (gs !== 'needs you (1), in progress (2), queued (40)') early.push(`groups "${gs}"`);
      const c = $('#queue .qc'), x = $('#queue .qx');
      // #391: the chain row is a row: the first step's number and title, `chain · 6`, its place, its columns where a queued
      // row has them, and `▸ #302 → #307 · next: #400 (in impl)` under them.
      const cs = c && c.querySelector('summary'), cells = s => [':scope>.gt', ':scope>span:first-of-type', ':scope>span:nth-of-type(2)', ':scope>.m', ':scope>.tl'].map(k => s.querySelector(k));
      const words = cs && [...cells(cs).slice(1), cs.querySelector('.cst')].map(x => x?.textContent).join(' | ');
      if (!c || c.open || words !== '#302 | chain · 6 | #3 | decoupling: split the serve module | #302 → #307 · next: #400 (in impl)' || getComputedStyle(cs.querySelector('.cst'), '::before').content !== '"▸ "' || [...c.querySelectorAll('details.q')].some(seen)) early.push(`chain row: ${words}`);
      const q1 = $('#queue .fc:not([hidden]) .qg[data-g=queue]>details.q'), at = s => cells(s).map(x => x && `${x.className}@${Math.round(x.getBoundingClientRect().left)}`).join();
      if (cs && at(cs) !== at(q1.querySelector('summary'))) early.push(`chain row columns ${at(cs)}, a queued row's ${at(q1.querySelector('summary'))}`);
      if (!x || x.open || x.querySelector('summary').textContent !== '+ 30 more' || [...x.querySelectorAll('details.q')].some(seen)) early.push(`more row: ${x && x.querySelector('summary').textContent}`);
      const rgb = s => s.match(/[\d.]+/g).map(Number), bg = e => { for (; e; e = e.parentElement) { const v = rgb(getComputedStyle(e).backgroundColor); if (v[3] !== 0) return v.slice(0, 3); } return rgb(getComputedStyle(B).backgroundColor); };
      for (const t of [...$$('#queue summary>.tl')].filter(seen)) if (ratio(rgb(getComputedStyle(t).color), bg(t)) < 4.5) early.push(`title "${t.textContent.slice(0, 20)}" under 4.5:1`);
    },
    // #367: the chain and `+ 30 more` opened in place (the steps right under the chain row, depth ≤ 2, the rest right under
    // `+ 30 more`), every title starting at one x; scrolled to the end, the last row clears the tab bar.
    'queue-chain': () => {
      tab('queue');
      $('.qch [data-qr="acme-core"]').click();
      const c = $('#queue .qc'), x = $('#queue .qx');
      c.open = x.open = true;
      const steps = [...c.querySelectorAll(':scope>details.q')], rest = [...x.querySelectorAll(':scope>*:not(summary)')];
      if (steps.length !== 6 || !steps.every(seen) || steps.some(d => !/\bd[012]\b/.test(d.className))) early.push(`chain opened ${steps.filter(seen).length} of ${steps.length} steps`);
      if (getComputedStyle(c.querySelector('summary>.cst'), '::before').content !== '"▾ "') early.push('an open chain row shows no ▾');
      const under = (a, b) => Math.abs(b.getBoundingClientRect().top - a.getBoundingClientRect().bottom) > 1;
      if (under(c.querySelector('summary'), steps[0]) || rest.length !== 30 || !rest.every(seen) || under(x.querySelector('summary'), rest[0])) early.push('chain or more not opened in place');
      const xs = new Set([...$$('#queue summary>.tl')].filter(seen).map(t => Math.round(t.getBoundingClientRect().left + parseFloat(getComputedStyle(t).paddingLeft))));
      if (xs.size !== 1) early.push(`titles start at ${[...xs].join(', ')}`);
      scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' });
      const last = [...$$('#queue .fc:not([hidden]) summary')].filter(seen).at(-1).getBoundingClientRect().bottom, nav = $('nav').getBoundingClientRect().top;
      if (last > nav) early.push(`last row ends at ${last}, under the tab bar at ${nav}`);
      scrollTo({ top: 0, behavior: 'instant' }); // a scrolled view shoots blank: the shot is the top, the needs cards hidden
      if (!DBG.get('check')) $('#needs').hidden = true;
    },
    // #365 gate 2 (the `queue` fixture: 100 open issues in acme-api's backlog): search, a quick filter and a label narrow
    // the rows; three ticks show [queue 3] above the tab bar with the issues in tick order; no row is wider than the screen.
    'queue-backlog': () => {
      tab('queue');
      $('.qch [data-qr="acme-api"]').click();
      const b = $('#queue .fc:not([hidden]) .bl'), rows = () => [...b.querySelectorAll('.bi')].filter(seen), n = () => b.querySelector('summary>.m').textContent;
      b.open = true;
      const all = rows().length, q = b.querySelector('.bs input'), f = b.querySelector('.bbar'), nav = $('nav').getBoundingClientRect().top;
      if (all !== 100 || n() !== '(100)') early.push(`backlog shows ${all} rows, ${n()}`);
      Object.assign(q, { value: 'past one' }).dispatchEvent(new Event('input', { bubbles: true }));
      if (!rows().length || rows().length === all || rows().some(r => !r.textContent.includes('past one')) || n() !== `(${rows().length} of 100)`) early.push(`search: ${rows().length} rows, ${n()}`);
      Object.assign(q, { value: '' }).dispatchEvent(new Event('input', { bubbles: true }));
      b.querySelector('[data-bf="90"]').click();
      const is = JSON.parse(b.dataset.bl), old = Number(document.body.dataset.ts) - 90 * 86400;
      if (!rows().length || rows().some(r => Date.parse(is[r.dataset.n].created) / 1000 > old + 86400) || b.querySelector('[data-bf="90"]').getAttribute('aria-pressed') !== 'true') early.push(`>90 days: ${rows().length} rows`);
      b.querySelector('[data-bf="90"]').click();
      const sel = b.querySelector('.bfs select');
      Object.assign(sel, { value: 'phone' }).dispatchEvent(new Event('change', { bubbles: true }));
      if (!rows().length || rows().some(r => !JSON.parse(b.dataset.bl)[r.dataset.n].labels.includes('phone')) || !sel.closest('.pk').textContent.startsWith('phone')) early.push(`label phone: ${rows().length} rows`);
      Object.assign(sel, { value: '' }).dispatchEvent(new Event('change', { bubbles: true }));
      const [x, y, z] = rows();
      for (const r of [z, x, y]) r.querySelector('input').click();
      const want = [z, x, y].map(r => r.dataset.n).join();
      if (!seen(f) || f.elements.issues.value !== want || f.querySelector('button').textContent !== 'queue 3') early.push(`bar: ${seen(f)} "${f.elements.issues.value}" (want ${want}) "${f.querySelector('button').textContent}"`);
      if (f.getBoundingClientRect().bottom > nav + 1) early.push(`[queue 3] ends at ${f.getBoundingClientRect().bottom}, under the tab bar at ${nav}`);
    },
    'queue-scroll': () => { tab('queue'); $$('#queue details.q').forEach(d => { d.open = true; }); },
    // #66: a fixture ▲ POST (the redirect a live server sends), through the page's fetch-submit path;
    // #140: then [Drive now], which must toast too and never navigate.
    toast: () => {
      tab('queue');
      href = location.href;
      const msg = o => ({ drive: 'drive started for ' + o.body.get('fleet'), mv: 'moved #12 before #18' })[o.body.get('op')] || 'error: empty POST body';
      window.fetch = (u, o) => Promise.resolve(Object.defineProperties(new Response(''), { redirected: { value: true }, url: { value: location.origin + '/?msg=' + encodeURIComponent(msg(o)) } }));
      // The demo disables every input (no writes), which would leave the POST body empty: enable the form's, as live.
      const submit = op => { const f = $(`#queue input[name=op][value=${op}]`).form; f.inert = false; [...f.elements].forEach(e => { e.disabled = false; }); f.requestSubmit(); return f; };
      submit('mv');
      setTimeout(() => { toasted = $('.msg.g') && $('.msg.g').textContent; drove = 'drive started for ' + submit('drive').elements.fleet.value; }, 500);
      setTimeout(() => { const m = $('.msg.g, .msg.r'); drove = m && m.textContent === drove || `want "${drove}", saw "${m && m.textContent}"`; }, 1000);
    },
  };
  async function kitchen(wx) {
    herd(true);
    await wait();
    await wait();
    $(`.hwx button[value=${wx}]`).click();
    const lv = { calm: 'calm', gusty: 'busy', storm: 'held' }[wx], s = getComputedStyle(H);
    if ($('#hp1 .horse') || !$('.hr .rs .h-held') || !H.classList.contains('heat-' + lv)) early.push(`kitchen: not its chefs and stations, or not ${lv} (#herd "${H.className}")`);
    if (H.matches('.night') || !lb.hidden) early.push('kitchen: night or the [A] light shown');
    if (lv === 'held' && (!$('.hh.idle use[href="#hfan"]') || getComputedStyle($('.hr .rs .ksmoke')).display === 'none')) early.push('held: no fanning chef or no smoke');
    if (V === 'herd-kitchen-still' && (!matchMedia('(prefers-reduced-motion: reduce)').matches || s.animationName !== 'none')) early.push(`still: #herd animates (${s.animationName})`);
    const t = $('.hqh.n span');
    if (t && t.scrollWidth > t.clientWidth) early.push('"! needs you" overflows its ticket');
  }
  async function garden(wx) {
    herd(true);
    await wait();
    await wait();
    $(`.hwx button[value=${wx}]`).click();
    const lv = { calm: 'calm', gusty: 'busy', storm: 'held' }[wx], l = { calm: 'l-sun', busy: 'l-overcast', held: 'l-rain' }[lv], s = getComputedStyle(H);
    if ($('#hp1 .horse') || !H.classList.contains('light-' + lv) || H.matches('.heat, .night') || !lb.hidden) early.push(`garden: not ${lv} light (#herd "${H.className}"), or night / [A]`);
    const shown = [...$$('.hr .rs svg.t0 :is(.l-sun,.l-overcast,.l-rain)')].filter(e => getComputedStyle(e).display !== 'none').map(e => e.classList[0]);
    if (!shown.length || shown.some(c => c !== l)) early.push(`garden ${lv}: the window shows ${[...new Set(shown)]}, not ${l}`);
    const lanes = [...hp(0).querySelectorAll('.hh')];
    if (!lanes.length || lanes.some(h => !h.querySelector('use[href^="#hs-plant-"]') || !h.querySelector('use[href^="#ha-"], use[href="#hm-fail"]'))) early.push('garden: a lane without the keeper over its plant');
    if (!$$('.hqh use[href^="#hs-plant-queued-"]').length) early.push('garden: no cutting in a jar on the counter');
    if (V === 'herd-garden-still' && (!matchMedia('(prefers-reduced-motion: reduce)').matches || s.animationName !== 'none')) early.push(`still: #herd animates (${s.animationName})`);
  }
  let toasted = null, drove = null, href = null;
  const ERR = 'error: drover: herdr agent start failed: {"error":{"code":"invalid_agent_argument","message":"agent arguments cannot be encoded safely for the target shell"},"id":"cli:agent:start"}';
  const dragged = [];
  const HP = { herd1: 0, herd2: 1, 'herd-kitchen': 0, 'herd-kitchen-busy': 0, 'herd-kitchen-held': 0, 'herd-kitchen-still': 0, 'herd-garden': 0, 'herd-garden-busy': 0, 'herd-garden-held': 0, 'herd-garden-still': 0, 'herd-nodone': 0, 'herd-night': 0, 'herd-night-storm': 0, 'herd-nav': 1 }; // the views measuring one Herd page
  const ROOTS = { 'loading-tap': () => [$('#sheet')], sheet: () => [$('#sheet')], 'sheet-full': () => [$('#sheet')], 'sheet-acp': () => [$('#sheet')], 'sheet-parked': () => [$('#sheet')], 'sheet-other': () => [$('#sheet')], ...Object.fromEntries(['chat-viewer', 'chat-viewer-zoom', 'chat-viewer-multi'].map(v => [v, () => [$('.ivb')]])), ...Object.fromEntries(Object.entries(HP).map(([v, n]) => [v, () => [hp(n), $('.hlt'), $('.hx'), $('.hwx')]])) };
  const CTL = 'button:not(.lr,.hh,.hqh), .pk, [role=button]', ROW = 'summary, .lr, .hh, .hqh, .ql, .lko', NAME = '.nw, .hl b';
  const CHOICE = 'button[name=choice], .pick'; // amendment A (#139: wherever it renders): wraps, ≤ 4 lines, instead of 28 px
  // A closed <details> keeps its content's boxes (content-visibility), so ask the browser what is shown.
  const seen = e => e.checkVisibility({ visibilityProperty: true });
  const label = e => (e.getAttribute('aria-label') || e.textContent).trim().replace(/\s+/g, ' ').slice(0, 24) || e.tagName.toLowerCase();
  function check() {
    let res;
    // #281: the row sprites' boxes (css px, viewport) as the screenshot shows them, above the fixed tab bar (one under it
    // would take in the bar's text); ui-audit.sh checks their pixels are crisp.
    const nav = $('nav').getBoundingClientRect().top;
    const sprites = [...$$('.rsg:not(.k)>.rsp:not(.rsa)')].filter(e => seen(e) && (!sysOn() || SY.contains(e))).map(e => e.getBoundingClientRect()).filter(r => r.top >= 0 && r.bottom <= nav).map(r => [r.left, r.top, r.width, r.height]);
    try { res = (SCROLL[V] || measure)(); } catch (e) { res = { view: V, controls: 0, outliers: ['audit error: ' + e.message] }; }
    const W = +(DBG.get('w') || 390); // #337: `&w=1440`, a desktop shot
    if (innerWidth !== W) res.outliers.push(`viewport ${innerWidth} px wide, not ${W}`); // #68: new-headless Chrome keeps windows ≥ 500 px
    // #114 app-like: no pinch or double-tap zoom.
    const vp = $('meta[name=viewport]').content, ta = getComputedStyle(document.documentElement).touchAction;
    if (vp !== 'width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no,viewport-fit=cover') res.outliers.push(`viewport meta "${vp}"`);
    if (ta !== 'manipulation') res.outliers.push(`html touch-action ${ta}, not manipulation`);
    if (['queue', 'lanes', 'host', 'events', 'queue-rows', 'events-rows', 'ascii-queue', 'ascii-lanes', 'ascii-host', 'ascii-events'].includes(V)) Object.assign(res, { sprites }).outliers.push(...drawn());
    // #331: every Queue row at every depth (the demo has a 4-deep chain) shows its whole title on one ellipsised line.
    if (V === 'queue') {
      if (![...$$('#queue summary .m')].some(e => e.textContent.startsWith('after #'))) res.outliers.push('no clamped (4-deep) chain row');
      for (const s of [...$$('#queue details.q>summary')].filter(seen)) { // #367: the picked repo's, outside closed chains
        const t = s.querySelector(':scope>.tl');
        if (!t || !seen(t) || t.title !== t.textContent || t.offsetHeight > 1.5 * parseFloat(getComputedStyle(t).lineHeight)) res.outliers.push(`no one-line title: ${label(s)}`);
      }
    }
    // #397: the error shows whole (wraps, never ellipsised) and covers no row: "all repos" starts under it.
    if (V === 'chat-error') {
      const m = $('.msg.r'), row = $('#chat .fc');
      if (!m || m.textContent !== ERR) res.outliers.push(`error banner reads "${m && m.textContent}"`);
      else if (m.scrollWidth > m.clientWidth || m.scrollHeight > m.clientHeight) res.outliers.push(`error banner clipped: ${m.scrollWidth}x${m.scrollHeight} in ${m.clientWidth}x${m.clientHeight}`);
      else if (m.getBoundingClientRect().bottom > row.getBoundingClientRect().top) res.outliers.push(`error banner covers "${label(row)}"`);
    }
    // #406: the viewer is open over the thread, its image loaded; fitted inside the screen, zoomed past it; 2 / 2 of two.
    if (V.startsWith('chat-viewer')) {
      const i = $('#iv>img'), r = i.getBoundingClientRect(), fit = r.left >= -1 && r.right <= innerWidth + 1 && r.top >= -1 && r.bottom <= innerHeight + 1;
      if ($('#iv').hidden || !i.complete || !i.naturalWidth) res.outliers.push(`viewer ${$('#iv').hidden ? 'closed' : 'open'}, image ${i.naturalWidth ? 'loaded' : 'not loaded'}`);
      if (fit !== (V !== 'chat-viewer-zoom')) res.outliers.push(`image ${r.left | 0}..${r.right | 0} × ${r.top | 0}..${r.bottom | 0}: ${fit ? 'fitted' : 'past the screen'}`);
      if (V === 'chat-viewer-multi' && $('.ivn').textContent !== '2 / 2') res.outliers.push(`count "${$('.ivn').textContent}", not 2 / 2`);
    }
    const out = document.createElement('pre');
    out.id = 'audit';
    out.textContent = JSON.stringify(res);
    B.append(out);
  }
  // #241 gate 2: the phone's theme on the tab. Terminal is today's glyph board: no sprite, no load scene, no mode button,
  // glyph avatars. Others draw a 16 px action sprite for each Queue/Events glyph (its title and label the plain word), a
  // load scene on each Host group header, and offer [scene] [ascii].
  function drawn() {
    // #440: System's Hosts & health and Activity are the old Host and Events tabs; their theme line is the section head.
    const sec = sysOn() && $('.sxi a.on')?.dataset.s, t = document.documentElement.dataset.theme, bad = [];
    const tb = sec ? { hosts: 'host', activity: 'events' }[sec] || sec : document.documentElement.dataset.tab;
    const on = s => [...$$(s)].filter(seen);
    if (!TT) return [`theme ${t} never loaded`];
    if (t === 'terminal') {
      if ($$('.rsp, .lsc, #mo').length) bad.push('terminal draws sprites');
      if ($$('.thw, .pw, .tsub, .lsm, [data-mo]').length) bad.push('terminal shows theme words'); // #370
      if (B.dataset.mode !== 'glyph' || $$('#mode').length || on('.av .px, .av .asc, .rsa, .lsa').length) bad.push(`terminal Lanes: mode ${B.dataset.mode}, or a mode button or sprite avatar shows`);
      return bad;
    }
    if (tb === 'queue' && !on('#queue .gt>b.rsg>.rsp').length) bad.push('no Queue row sprite');
    // #370 gate 2: the tab's own line and its labels pair the theme word with the plain one, and the muted plain word is
    // ≥ 4.5:1 on what is under it (the first ancestor with a background, over --bg when see-through).
    if (!on(sec ? `#sy-${sec}>.syh .thw` : `#${tb}>.tsub>.thw`).length) bad.push(`${tb}: no theme line`);
    const LBL = { queue: '#queue .qg>h3, #queue details.done>summary>span, #needs .ask>p:first-of-type>b', lanes: '#lanes h2>span', events: '#events details.er:is([data-k=merged],[data-k=verdict],[data-k=spawn],[data-k=queue]) .sm', host: '#host .hcd .hhd .hnm' }[tb];
    for (const e of on(LBL)) if (!e.querySelector('.thw') || !e.querySelector('.pw') && tb !== 'events') bad.push(`${tb}: label without its theme word: "${e.textContent.slice(0, 30)}"`);
    const rgb = c => { const v = c.match(/[\d.]+/g).map(Number); return c.startsWith('color(') ? [...v.slice(0, 3).map(x => x * 255), v[3] ?? 1] : [...v.slice(0, 3), v[3] ?? 1]; };
    const under = e => { for (; e; e = e.parentElement) { const c = rgb(getComputedStyle(e).backgroundColor); if (c[3] > 0) return c[3] < 1 ? over(c.slice(0, 3), rgb(getComputedStyle(B).backgroundColor), c[3]) : c.slice(0, 3); } return rgb(getComputedStyle(B).backgroundColor); };
    for (const e of on('.pw')) { const x = ratio(rgb(getComputedStyle(e).color).slice(0, 3), under(e)); if (x < 4.5) bad.push(`plain word "${e.textContent}" ${x.toFixed(2)}:1, under 4.5`); }
    if (tb === 'events' && !on('#events .kg.rsg>.rsp').length) bad.push('no Events row sprite');
    if (tb === 'host' && $$('#host>.grp>summary').length !== $$('#host>.grp>summary>.lsc').length) bad.push('a Host group without its load scene');
    if ($$('#mode').length || TM.join() !== 'scene,ascii') bad.push(`Lanes modes ${TM}, a mode button ${$$('#mode').length}`);
    // #335: ascii style draws the theme's figures, never a sprite; pixel style never a figure.
    const asc = B.dataset.mode === 'ascii';
    if (asc ? on('.rsg>svg, svg.lsc, .av .px').length : on('.rsa, .lsa').length) bad.push(`${B.dataset.mode}: a ${asc ? 'sprite' : 'figure'} of the other style`);
    if (asc && tb === 'lanes' && !on('.av .asc').length) bad.push('ascii: no ascii Lanes avatar');
    // #423: a theme with `stages` draws each Queue row's plant, and each Lanes avatar's keeper over its issue's plant.
    if (TT.stages && !asc && tb === 'queue' && !$$('#queue .rsg>.rsp use[href^="#hq-plant-"]').length) bad.push('no Queue row plant');
    if (TT.stages && !asc && tb === 'lanes' && !$$('#lanes .av .px use[href^="#hs-plant-"] + use[href^="#ha-"]').length) bad.push('no Lanes keeper over a plant');
    for (const g of on('.rsg')) if (!g.title || g.getAttribute('aria-label') !== g.title) bad.push(`a row sprite without its word: ${g.outerHTML.slice(0, 60)}`);
    // #281: a row sprite is 24×24 in a fixed 28 px slot, and the row with it is at most 4 px taller than with its glyph (no wrap).
    for (const g of on('.rsg')) {
      const s = g.firstChild.getBoundingClientRect(), w = g.getBoundingClientRect().width, sum = g.closest('summary'), h = sum.offsetHeight, svg = g.firstChild;
      if (Math.round(s.width) !== 24 || Math.round(s.height) !== 24 || Math.round(w) !== 28) bad.push(`row sprite ${s.width}×${s.height} in a ${w} px slot, not 24×24 in 28`);
      g.classList.remove('rsg');
      g.textContent = g.dataset.g;
      const d = h - sum.offsetHeight;
      g.replaceChildren(svg);
      g.classList.add('rsg');
      if (d > 4) bad.push(`a row ${d} px taller with its sprite than its glyph: ${label(sum)}`);
    }
    return bad;
  }
  function measure() {
    $$('[inert]').forEach(e => { e.inert = false; });
    const roots = (ROOTS[V] || (() => [sysOn() ? SY : B]))(), bad = []; // #440: System open, it alone is on screen
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
        // Text past its box's sides, unless that box ellipsises or scrolls it (the button's `]` ring is not text).
        let b = e;
        while (['inline', 'contents'].includes(getComputedStyle(b).display)) b = b.parentElement;
        const g = document.createRange();
        g.selectNodeContents(t);
        const tr = g.getBoundingClientRect(), br = b.getBoundingClientRect();
        if (!input && (tr.right > br.right + 1 || tr.left < br.left - 1) && getComputedStyle(b).textOverflow !== 'ellipsis' && !/auto|scroll/.test(getComputedStyle(b).overflowX)) bad.push(`"${t.data.trim().slice(0, 20)}": text overflows its box by ${Math.max(tr.right - br.right, br.left - tr.left) | 0}px`);
      }
      // #68: no box holds more than it shows unless it ellipsises (or scrolls by design); nothing past the screen's right edge.
      // Tap rings (`]`, the queued horse's ::before, a picker's invisible select grown 8 px each side) are hit area, not content:
      // off while measuring. The sky's clouds drift past its clipped sides.
      // #162 A: a summary's widest text box is at least half its row (a 7ch grid column once squeezed a 1-line summary into
      // 14); a text run straight in a grid/flex summary counts only when it wraps.
      for (const s of [...r.querySelectorAll('summary')].filter(seen)) {
        const box = e => { while (['inline', 'contents'].includes(getComputedStyle(e).display)) e = e.parentElement; return e.getBoundingClientRect().width; };
        const run = t => { const g = document.createRange(); g.selectNodeContents(t); return new Set([...g.getClientRects()].map(x => Math.round(x.top))).size > 1 ? g.getBoundingClientRect().width : Infinity; };
        const texts = [...s.querySelectorAll('*'), s].flatMap(e => [...e.childNodes].filter(n => n.nodeType === 3 && n.data.trim()));
        const w = Math.max(0, ...texts.map(t => t.parentElement === s ? run(t) : box(t.parentElement)));
        if (texts.length && w < s.getBoundingClientRect().width / 2) bad.push(`summary "${s.textContent.trim().slice(0, 24)}": widest text box ${w | 0}px of its ${s.getBoundingClientRect().width | 0}px row`);
      }
      const ring = Object.assign(document.createElement('style'), { textContent: 'button::after,.pk::after,.hqh::before{display:none!important}.pk select{inset:0!important;width:100%!important}' });
      B.append(ring);
      for (const e of [r, ...r.querySelectorAll('*')]) {
        if (e.closest('svg, .hr') || e.parentElement?.closest('.sxi') || !seen(e)) continue; // #440: System's index scrolls sideways
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
      for (const e of [p.querySelector('.hs'), $('.hlt'), $('.hx')]) if (!e.hidden && e.offsetHeight !== 28) bad.push(`${e.className}: Herd strip/[A]/[×] ${e.offsetHeight}px, not 28`);
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
      if (drove !== true) bad.push(`[Drive now] gave no toast: ${drove}`);
      if (location.href !== href) bad.push(`[Drive now] navigated: ${location.href}`);
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
    // #516: herd.json repeating a lane's name (one copy in another band), and a stale copy of its card already on the
    // page: after 5 refreshes every name has one card, the page none more, and "N here" counts the cards.
    'herd-dupes': () => {
      const base = structuredClone(data), bad = [], f = base.fleets[0], l = f.lanes.find(x => !x.gone);
      f.lanes.push({ ...l }, { ...l, state: 'idle', shown: 'idle' });
      herdData(structuredClone(base));
      const p = hp(0), names = () => [...p.querySelectorAll('.hh')].map(e => e.dataset.agent);
      const c = p.querySelector(`.hh[data-agent="${CSS.escape(l.name)}"]`);
      c.after(c.cloneNode(true));
      const n = [];
      for (let i = 0; i < 5; i++) { herdData(structuredClone(base)); n.push(names().length); }
      const ks = names(), want = new Set(f.lanes.filter(x => !x.gone).map(x => x.name)).size;
      if (new Set(ks).size !== ks.length) bad.push(`a name drawn twice: ${ks.join(' ')}`);
      if (n.some(x => x !== want)) bad.push(`cards per refresh ${n.join(',')}, not ${want} each`);
      const here = p.querySelector('.hs .n').textContent;
      if (here !== `· ${want} here`) bad.push(`"${here}", not ${want} here`);
      return { view: V, controls: ks.length, outliers: bad };
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
  VIEWS['row-loop-still'] = VIEWS['row-loop'];
  VIEWS['moment-row-still'] = VIEWS['moment-row'];
  const go = async () => {
    await themed; // #241: the phone's theme drawn first
    await window.boardReady; // #266: the TypeScript board's first draw (Events)
    await VIEWS[V]();
    setTimeout(() => {
      if (V === 'herd2') PG.scrollLeft = PG.clientWidth;
      setTimeout(() => { if (DBG.get('check')) check(); }, 1000);
    }, V === 'toast' ? 5000 : 2000);
  };
  if (document.readyState === 'complete') go(); else addEventListener('load', go);
})();

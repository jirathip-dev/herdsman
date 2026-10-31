// #58 amendment B control audit, run in the page (tests/ui_audit.rs, WebKit at 390 px): every visible
// control under `root` is 28 px tall, 11 or 13 px type, unfilled, and its 44 px tap target (an 8 px
// ring) answers elementFromPoint 7 px outside each edge; inputs are 16 px. Throws with every outlier.
function audit(root) {
  const bad = [], seen = e => e.getClientRects().length > 0, R = document.querySelector(root);
  R.querySelectorAll('details').forEach(d => { d.open = true; });
  const ctl = [...R.querySelectorAll('button:not(.lr,.hh,.hqh), .pk')].filter(seen);
  for (const e of ctl) {
    const cs = getComputedStyle(e), name = (e.getAttribute('aria-label') || e.textContent).trim().slice(0, 24);
    if (cs.height !== '28px') bad.push(`${name}: height ${cs.height}`);
    if (!['11px', '13px'].includes(cs.fontSize)) bad.push(`${name}: font ${cs.fontSize}`);
    if (!['rgba(0, 0, 0, 0)', 'transparent'].includes(cs.backgroundColor)) bad.push(`${name}: background ${cs.backgroundColor}`);
    e.scrollIntoView({ block: 'center', inline: 'nearest' });
    const r = e.getBoundingClientRect(), cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    for (const [x, y] of [[cx, r.top - 7], [cx, r.bottom + 7], [r.left - 7, cy], [r.right + 7, cy]]) {
      if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) continue;
      const h = document.elementFromPoint(x, y);
      if (!h || !e.contains(h)) bad.push(`${name}: (${x | 0},${y | 0}) hits ${h ? h.outerHTML.slice(0, 60) : 'nothing'}`);
    }
  }
  for (const i of [...R.querySelectorAll('input:not([type=hidden]), textarea')].filter(seen)) {
    if (getComputedStyle(i).fontSize !== '16px') bad.push(`${i.name}: input font ${getComputedStyle(i).fontSize}`);
  }
  scrollTo(0, 0);
  if (bad.length) throw new Error('UI audit: ' + bad.join(' | '));
  return ctl.length;
}

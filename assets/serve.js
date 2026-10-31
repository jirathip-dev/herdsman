// Tabs, lane display mode (both kept in localStorage), the stale badge, 30 s refresh, SW,
// and the Lanes bottom sheet: card, transcript (5 s poll), reply box with one-time pairing.
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
$$('.lane .lr').forEach(b => b.onclick = () => open(b.parentNode));
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
setInterval(() => { if (!$('details[open]') && !$('select:focus') && S.hidden) location.reload(); }, 30000);
if ('serviceWorker' in navigator) navigator.serviceWorker.register('/sw.js').catch(() => {});

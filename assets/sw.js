// herdsman service worker: the page stale-while-revalidate (the cached copy paints first, the network
// refresh replaces it on a 200; no copy yet: the network answer as is), the `a/@V/` assets precached at
// install (immutable), other static files cache-first; the live /lane transcript, the /live stream,
// /herd.json, /frags and `?…` reads are never cached. A navigation to any other URL (#67: iOS restoring a fetch
// URL as the page) gets the page: the cached copy, else the network's.
// Paths are under the registration's scope (`/` live, e.g. `/herdsman/` for the Pages demo); one cache per scope.
const P = self.registration ? new URL(self.registration.scope).pathname : '/', C = 'herdsman-v6:' + P;
const A = ['serve.css', 'herd.css', 'serve.js', 'herd.js', 'horses.svg'].map(f => P + 'a/@V/' + f);
self.addEventListener('install', e => { self.skipWaiting(); e.waitUntil(caches.open(C).then(c => c.addAll(A)).catch(() => {})); });
self.addEventListener('activate', e => e.waitUntil(caches.keys().then(ks => Promise.all(ks.filter(k => k.endsWith(':' + P) && k !== C).map(k => caches.delete(k)))).then(() => self.clients.claim())));
self.addEventListener('fetch', e => {
  const u = new URL(e.request.url);
  if (e.request.method !== 'GET' || u.origin !== location.origin) return;
  if ([P + 'lane', P + 'live', P + 'herd.json', P + 'frags'].includes(u.pathname) || u.search) return;
  if (u.pathname === P) {
    const net = fetch(e.request).then(r => {
      const copy = r.clone();
      if (r.ok) caches.open(C).then(c => c.put(P, copy));
      return r;
    });
    e.respondWith(caches.match(P).then(m => m || net).catch(() => net));
    e.waitUntil(net.catch(() => {}));
  } else {
    e.respondWith(caches.match(e.request).then(m => m || fetch(e.request).then(r => {
      const copy = r.clone();
      if (r.ok) caches.open(C).then(c => c.put(e.request, copy));
      return r;
    })));
  }
});
// Web push (#18): show the alert; a tap opens (or focuses) the page on its tab and issue.
self.addEventListener('push', e => {
  const j = e.data ? e.data.json() : {};
  e.waitUntil(self.registration.showNotification(j.title || 'herdsman', { body: j.body || '', data: j.url || P }));
});
self.addEventListener('notificationclick', e => {
  e.notification.close();
  const url = e.notification.data || P;
  e.waitUntil(self.clients.matchAll({ type: 'window' }).then(ws => ws.length ? ws[0].navigate(url).then(w => w && w.focus()) : self.clients.openWindow(url)));
});

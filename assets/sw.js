// herdsman service worker: network-first for the page (offline: the last copy, which
// shows its own "last update" time), cache-first for the static assets; the live /lane
// transcript, the /live stream, /herd.json and `?…` reads (`?frag=all`) are never cached.
// Paths are under the registration's scope (`/` live, e.g. `/herdsman/` for the Pages demo); one cache per scope.
const P = self.registration ? new URL(self.registration.scope).pathname : '/', C = 'herdsman-v5:' + P;
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', e => {
  const u = new URL(e.request.url);
  if (e.request.method !== 'GET' || u.origin !== location.origin || [P + 'lane', P + 'live', P + 'herd.json'].includes(u.pathname) || u.search) return;
  if (u.pathname === P) {
    e.respondWith(fetch(e.request).then(r => {
      const copy = r.clone();
      if (r.ok) caches.open(C).then(c => c.put(P, copy));
      return r;
    }).catch(() => caches.match(P)));
  } else {
    e.respondWith(caches.match(e.request).then(m => m || fetch(e.request).then(r => {
      const copy = r.clone();
      caches.open(C).then(c => c.put(e.request, copy));
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

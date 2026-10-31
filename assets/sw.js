// herdsman service worker: network-first for the page (offline: the last copy, which
// shows its own "last update" time), cache-first for the static assets; the live /lane
// transcript, the /live stream, /herd.json and `?…` reads (`?frag=all`) are never cached.
const C = 'herdsman-v4';
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', e => {
  const u = new URL(e.request.url);
  if (e.request.method !== 'GET' || u.origin !== location.origin || u.pathname === '/lane' || u.pathname === '/live' || u.pathname === '/herd.json' || u.search) return;
  if (u.pathname === '/') {
    e.respondWith(fetch(e.request).then(r => {
      const copy = r.clone();
      if (r.ok) caches.open(C).then(c => c.put('/', copy));
      return r;
    }).catch(() => caches.match('/')));
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
  e.waitUntil(self.registration.showNotification(j.title || 'herdsman', { body: j.body || '', data: j.url || '/' }));
});
self.addEventListener('notificationclick', e => {
  e.notification.close();
  const url = e.notification.data || '/';
  e.waitUntil(self.clients.matchAll({ type: 'window' }).then(ws => ws.length ? ws[0].navigate(url).then(w => w && w.focus()) : self.clients.openWindow(url)));
});

// herdsman service worker: network-first for the page (offline: the last copy, which
// shows its own "last update" time), cache-first for the static assets; the live /lane
// transcript, the /live stream and `?…` reads (`?frag=all`) are never cached.
const C = 'herdsman-v3';
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', e => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', e => {
  const u = new URL(e.request.url);
  if (e.request.method !== 'GET' || u.origin !== location.origin || u.pathname === '/lane' || u.pathname === '/live' || u.search) return;
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

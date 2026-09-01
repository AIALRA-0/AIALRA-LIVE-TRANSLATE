const CACHE = "aialra-shell-v2";
const SHELL = ["/app", "/icon.svg", "/manifest.webmanifest"];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(caches.keys().then((keys) => Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key)))).then(() => self.clients.claim()));
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  const isAppNavigation = request.mode === "navigate" && (url.pathname === "/" || url.pathname.startsWith("/app"));
  const isStaticShell = url.pathname.startsWith("/assets/") || ["/icon.svg", "/manifest.webmanifest"].includes(url.pathname);
  if (request.method !== "GET" || url.origin !== self.location.origin || (!isAppNavigation && !isStaticShell)) return;
  event.respondWith(fetch(request).then((response) => {
    if (response.ok) caches.open(CACHE).then((cache) => cache.put(request, response.clone()));
    return response;
  }).catch(async () => {
    const cached = await caches.match(request);
    if (cached) return cached;
    if (request.mode === "navigate") return caches.match("/app");
    throw new Error("offline static resource unavailable");
  }));
});

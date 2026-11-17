// SaveSpace service worker: shows Web Push notifications (design doc D13).
// It does not cache pages: SaveSpace shows current money data only.

self.addEventListener("push", (event) => {
  let data = { title: "SaveSpace", body: "There is something new in SaveSpace.", url: "/" };
  try {
    data = { ...data, ...event.data.json() };
  } catch {
    // Keep the general text.
  }
  event.waitUntil(
    self.registration.showNotification(data.title, {
      body: data.body,
      icon: "/favicon.svg",
      data: { url: data.url },
      tag: "savespace",
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = new URL(event.notification.data?.url ?? "/", self.location.origin);
  // Only open pages of this app.
  const target = url.origin === self.location.origin ? url.href : self.location.origin;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      for (const w of windows) {
        if (w.url.startsWith(self.location.origin) && "focus" in w) {
          w.navigate(target);
          return w.focus();
        }
      }
      return self.clients.openWindow(target);
    }),
  );
});

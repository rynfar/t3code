(() => {
  let delivering = false;
  const setColorScheme = (theme) => {
    if (theme !== "light" && theme !== "dark") return;
    let meta = document.querySelector('meta[name="color-scheme"]');
    if (!meta) {
      meta = document.createElement("meta");
      meta.name = "color-scheme";
      (document.head || document.documentElement)?.append(meta);
    }
    if (meta) meta.content = theme;
  };
  window.__t3McpReceive = (data) => {
    if (data?.jsonrpc === "2.0" && data.method === "ui/notifications/host-context-changed") {
      setColorScheme(data.params?.theme);
    }
    delivering = true;
    try {
      window.dispatchEvent(new MessageEvent("message", { data, source: window }));
    } finally {
      delivering = false;
    }
  };
  // The SDK's parent is this top-level window; outgoing messages must not reach its listeners.
  window.addEventListener(
    "message",
    (event) => {
      if (!delivering && event.source === window && event.data?.jsonrpc === "2.0") {
        event.stopImmediatePropagation();
        let serialized;
        try {
          serialized = JSON.stringify(event.data);
        } catch {
          const id = event.data.id;
          if (
            (typeof id === "string" || typeof id === "number") &&
            typeof event.data.method === "string"
          ) {
            window.__t3McpReceive({
              jsonrpc: "2.0",
              id,
              error: { code: -32602, message: "Params must be JSON." },
            });
          }
          return;
        }
        // oxlint-disable-next-line unicorn/require-post-message-target-origin -- WebKit IPC takes one serialized argument.
        window.ipc.postMessage(serialized);
      }
    },
    true,
  );
})();

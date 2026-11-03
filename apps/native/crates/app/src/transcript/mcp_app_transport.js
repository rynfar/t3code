(() => {
  let delivering = false;
  window.__t3McpReceive = (data) => {
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
        // oxlint-disable-next-line unicorn/require-post-message-target-origin -- WebKit IPC takes one serialized argument.
        window.ipc.postMessage(JSON.stringify(event.data));
      }
    },
    true,
  );
})();

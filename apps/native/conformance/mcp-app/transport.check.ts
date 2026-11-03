import * as NodeAssert from "node:assert/strict";
import * as NodeFS from "node:fs";
import * as NodeTest from "node:test";
import * as NodeVM from "node:vm";

const bootstrap = NodeFS.readFileSync(
  new URL("../../crates/app/src/transcript/mcp_app_transport.js", import.meta.url),
  "utf8",
);

type Message = {
  jsonrpc: string;
  id?: number;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code: number; message: string };
};

class PageMessageEvent {
  data: Message;
  source: unknown;
  stopImmediatePropagation = () => {};
  constructor(_type: string, init: { data: Message; source: unknown }) {
    this.data = init.data;
    this.source = init.source;
  }
}

class PageWindow {
  parent = this;
  listeners: Array<{ callback: (event: PageMessageEvent) => void; capture: boolean }> = [];
  queued: PageMessageEvent[] = [];
  posted: unknown[] = [];
  ipc = { postMessage: (text: string) => this.posted.push(JSON.parse(text)) };
  __t3McpReceive!: (data: Message) => void;
  addEventListener(_type: string, callback: (event: PageMessageEvent) => void, capture = false) {
    this.listeners.push({ callback, capture });
  }
  dispatchEvent(event: PageMessageEvent) {
    let stopped = false;
    event.stopImmediatePropagation = () => {
      stopped = true;
    };
    for (const { callback } of [...this.listeners].sort(
      (a, b) => Number(b.capture) - Number(a.capture),
    )) {
      if (stopped) break;
      callback(event);
    }
  }
  postMessage(data: Message, _origin: string) {
    this.queued.push(new PageMessageEvent("message", { data, source: this }));
  }
  flush() {
    for (let event = this.queued.shift(); event; event = this.queued.shift())
      this.dispatchEvent(event);
  }
}

NodeTest.test("a top-level MCP app never receives its own requests or notifications", () => {
  const window = new PageWindow();
  const received: Message[] = [];
  const pending = new Map<number, Message | null>();
  NodeVM.runInNewContext(bootstrap, { window, MessageEvent: PageMessageEvent });
  // Same source check as ext-apps' PostMessageTransport, with the SDK's unsupported-request reply.
  window.addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const message = event.data;
    received.push(message);
    if (message.id !== undefined && message.method) {
      window.parent.postMessage(
        { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Method not found" } },
        "*",
      );
    } else if (message.id !== undefined && pending.has(message.id))
      pending.set(message.id, message);
  });
  const requests = [
    { jsonrpc: "2.0", id: 1, method: "ui/initialize", params: { protocolVersion: "2026-01-26" } },
    { jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "get_weather" } },
  ];
  for (const request of requests) {
    pending.set(request.id, null);
    window.parent.postMessage(request, "*");
    window.flush();
    NodeAssert.equal(
      pending.get(request.id),
      null,
      "the SDK rejected its own request before the host answered",
    );
    NodeAssert.equal(received.length, 0, "page-to-host messages must only reach IPC");
    NodeAssert.deepEqual(window.posted.splice(0), [request]);
    const reply = { jsonrpc: "2.0", id: request.id, result: { ok: true } };
    window.__t3McpReceive(reply);
    NodeAssert.deepEqual(pending.get(request.id), reply);
    NodeAssert.equal(window.posted.length, 0, "host responses must not echo back to IPC");
    received.length = 0;
  }
  const initialized = { jsonrpc: "2.0", method: "ui/notifications/initialized" };
  window.parent.postMessage(initialized, "*");
  window.flush();
  NodeAssert.deepEqual(window.posted.splice(0), [initialized]);
  NodeAssert.equal(received.length, 0);
  const notification = {
    jsonrpc: "2.0",
    method: "ui/notifications/tool-result",
    params: { content: [] },
  };
  window.__t3McpReceive(notification);
  NodeAssert.deepEqual(received, [notification]);
  NodeAssert.equal(window.posted.length, 0);
});

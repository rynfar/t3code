// Ports the trusted-input handlers in shared/src/htmlRender.ts for a top-level WebKit page.
(function () {
  if (window.parent !== window || !window.ipc) return;
  const post = (method, params, id) =>
    window.ipc.postMessage(
      JSON.stringify({ jsonrpc: "2.0", method, params, ...(id === undefined ? {} : { id }) }),
    );
  let shortcuts = [],
    blocked = [],
    codes = {},
    n = 0,
    theme;
  function apply() {
    const sheet = document.getElementById("t3-theme");
    if (!sheet || !theme) return false;
    let host = document.getElementById("t3-host-theme");
    if (!host) {
      host = document.createElement("style");
      host.id = "t3-host-theme";
      sheet.after(host);
    }
    const declarations = Object.entries(theme.variables)
      .filter(([key]) => /^--[a-z0-9-]+$/.test(key))
      .map(([key, value]) => key + ":" + String(value).replace(/[;{}<>]/g, "") + ";")
      .join("");
    host.textContent =
      ":root{color-scheme:" +
      theme.appearance +
      ";" +
      declarations +
      "}:where(img:not(a[href] img,button img)){cursor:zoom-in}";
    return true;
  }
  window.addEventListener("message", (e) => {
    const d = e.data,
      p = d && d.params;
    if (!d || d.jsonrpc !== "2.0" || !p) return;
    if (d.method === "ui/notifications/host-context-changed") {
      theme = { appearance: p.theme, variables: p.styles.variables };
      apply();
    }
    if (d.method === "t3/notifications/shortcuts") {
      shortcuts = p.shortcuts;
      blocked = p.blocked;
      codes = p.codes;
    }
  });
  const observer = new MutationObserver(() => {
    if (apply()) observer.disconnect();
  });
  observer.observe(document, { childList: true, subtree: true });
  document.addEventListener("DOMContentLoaded", () => {
    apply();
    observer.disconnect();
  });
  document.addEventListener(
    "click",
    (e) => {
      const link =
        e.isTrusted && e.composedPath().find((t) => t && t.matches && t.matches("a[href]"));
      if (!link) return;
      let url;
      try {
        url = new URL(link.getAttribute("href"), document.baseURI);
      } catch {
        return;
      }
      if (!/^https?:$/.test(url.protocol) || url.href.split("#")[0] === location.href.split("#")[0])
        return;
      e.preventDefault();
      post(
        "ui/open-link",
        { url: url.href, metaKey: e.metaKey, ctrlKey: e.ctrlKey },
        "t3-link-" + ++n,
      );
    },
    true,
  );
  window.addEventListener("click", (e) => {
    const t = e.composedPath()[0];
    if (
      !e.isTrusted ||
      e.defaultPrevented ||
      e.button !== 0 ||
      e.metaKey ||
      e.ctrlKey ||
      e.shiftKey ||
      e.altKey ||
      !t ||
      t.tagName !== "IMG" ||
      t.closest("a[href],button")
    )
      return;
    const src = t.currentSrc || t.src;
    if (!src) return;
    setTimeout(() => {
      if (!e.defaultPrevented) {
        post("t3/notifications/open-image", { src, name: t.alt || t.title || "" });
      }
    });
  });
  let scrollQueued = false;
  let lastScrollAreas = "";
  const pushScrollAreas = () => {
    scrollQueued = false;
    const areas = [];
    const root = document.scrollingElement || document.documentElement;
    if (root && root.scrollHeight > root.clientHeight + 1) {
      const oy = getComputedStyle(root).overflowY;
      if (oy !== "hidden" && oy !== "clip") {
        areas.push({
          left: 0,
          top: 0,
          right: Math.round(window.innerWidth),
          bottom: Math.round(window.innerHeight),
          canUp: root.scrollTop > 1,
          canDown: root.scrollTop + root.clientHeight < root.scrollHeight - 1,
        });
      }
    }
    const nodes = document.body ? document.body.getElementsByTagName("*") : [];
    const cap = Math.min(nodes.length, 2000);
    for (let i = 0; i < cap && areas.length < 64; i++) {
      const el = nodes[i];
      if (el.scrollHeight <= el.clientHeight + 1 || el.clientHeight <= 0 || el.clientWidth <= 0)
        continue;
      const oy = getComputedStyle(el).overflowY;
      if (oy !== "auto" && oy !== "scroll" && oy !== "overlay") continue;
      const r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) continue;
      areas.push({
        left: Math.round(r.left),
        top: Math.round(r.top),
        right: Math.round(r.right),
        bottom: Math.round(r.bottom),
        canUp: el.scrollTop > 1,
        canDown: el.scrollTop + el.clientHeight < el.scrollHeight - 1,
      });
    }
    const serialized = JSON.stringify(areas);
    if (serialized === lastScrollAreas) return;
    lastScrollAreas = serialized;
    post("t3/notifications/scroll-areas", { areas });
  };
  const queueScrollAreas = () => {
    if (scrollQueued) return;
    scrollQueued = true;
    requestAnimationFrame(pushScrollAreas);
  };
  document.addEventListener("DOMContentLoaded", queueScrollAreas);
  window.addEventListener("load", queueScrollAreas);
  window.addEventListener("resize", queueScrollAreas);
  document.addEventListener("scroll", queueScrollAreas, { capture: true, passive: true });
  if (window.MutationObserver) {
    new MutationObserver(queueScrollAreas).observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["style", "class", "open"],
    });
  }
  window.addEventListener(
    "keydown",
    (e) => {
      const t = e.composedPath()[0],
        code = e.code || "",
        key = e.key.toLowerCase();
      const keys = /^[a-z]$/.test(key)
        ? [key]
        : [key, /^Key[A-Z]$/.test(code) ? code.slice(3).toLowerCase() : "", codes[code] || ""];
      const matches = (s) =>
        s.metaKey === e.metaKey &&
        s.ctrlKey === e.ctrlKey &&
        s.shiftKey === e.shiftKey &&
        s.altKey === e.altKey &&
        keys.includes(s.key);
      if (
        !e.isTrusted ||
        e.isComposing ||
        (e.getModifierState("AltGraph") && !/^[a-z0-9]$/i.test(e.key)) ||
        (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) ||
        !shortcuts.some(matches) ||
        blocked.some(matches)
      )
        return;
      const forward = (v) => {
        window.removeEventListener("keydown", forward);
        if (v !== e || e.defaultPrevented) return;
        e.preventDefault();
        post("t3/notifications/keydown", {
          key: e.key,
          code: e.code,
          metaKey: e.metaKey,
          ctrlKey: e.ctrlKey,
          shiftKey: e.shiftKey,
          altKey: e.altKey,
          repeat: e.repeat,
        });
      };
      window.addEventListener("keydown", forward);
      setTimeout(() => window.removeEventListener("keydown", forward));
    },
    true,
  );
})();

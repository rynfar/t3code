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
    host.textContent = ":root{color-scheme:" + theme.appearance + ";" + declarations + "}";
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
      if (!/^https?:$/.test(url.protocol)) return;
      e.preventDefault();
      post(
        "ui/open-link",
        { url: url.href, metaKey: e.metaKey, ctrlKey: e.ctrlKey },
        "t3-link-" + ++n,
      );
    },
    true,
  );
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

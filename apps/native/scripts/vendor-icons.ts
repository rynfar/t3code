// Vendors the Lucide icons `crates/app/src/icons.rs` names with the web's stroke baked in
// (design-system §8): 1.75, or 2 under `stroke-2/`. `--providers` vendors provider brand marks.
// Usage: node apps/native/scripts/vendor-icons.ts [name | stroke-2/name]... | --providers
//        node apps/native/scripts/vendor-icons.ts --check   (every file is present and current)
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

const ROOT = NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "../../..");
const OUT = NodePath.join(ROOT, "apps/native/crates/app/assets/icons/lucide");
const ICONS_RS = NodePath.join(ROOT, "apps/native/crates/app/src/icons.rs");
const require = NodeModule.createRequire(NodePath.join(ROOT, "apps/web/package.json"));

type IconNode = ReadonlyArray<readonly [string, Readonly<Record<string, string>>]>;

const LUCIDE = NodePath.dirname(require.resolve("lucide/package.json"));

function lucideIcon(name: string): IconNode {
  const file = NodePath.join(LUCIDE, "dist/esm/lucide/src/icons", `${name}.js`);
  const source = NodeFS.readFileSync(file, "utf8");
  const body = source.slice(source.indexOf("["), source.lastIndexOf("]") + 1);
  return Function(`"use strict"; return (${body});`)() as IconNode;
}

function toSvg(node: IconNode, stroke: string): string {
  const children = node
    .map(([tag, attrs]) => {
      const rendered = Object.entries(attrs)
        .filter(([key]) => key !== "key")
        .map(([key, value]) => `${key}="${value}"`)
        .join(" ");
      return `  <${tag} ${rendered}/>`;
    })
    .join("\n");
  return `<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="${stroke}" stroke-linecap="round" stroke-linejoin="round">\n${children}\n</svg>\n`;
}

function namesInIconsRs(): string[] {
  const source = NodeFS.readFileSync(ICONS_RS, "utf8");
  return [...source.matchAll(/=> "icons\/lucide\/((?:stroke-2\/)?[a-z0-9-]+)\.svg"/g)].map(
    (match) => match[1]!,
  );
}

// Provider brand marks (`ProviderInstanceIcon.tsx`): the logos in `Icons.tsx`, and each provider
// package's `icon` with its light and dark fills baked into one file per theme.
const PROVIDERS_OUT = NodePath.join(ROOT, "apps/native/crates/app/assets/icons/providers");
const ICONS_TSX = NodePath.join(ROOT, "apps/web/src/components/Icons.tsx");
const PROVIDER_CLIENTS = NodePath.join(ROOT, "packages/client-runtime/src/providerClients.ts");
const SVG_NS = 'xmlns="http://www.w3.org/2000/svg"';

interface PackageIcon {
  readonly viewBox: string;
  readonly fill: { readonly light: string; readonly dark: string };
  readonly paths: ReadonlyArray<{
    readonly d: string;
    readonly fillRule?: string;
    readonly fill?: { readonly light: string; readonly dark: string };
    readonly gradient?: {
      readonly x1: number;
      readonly y1: number;
      readonly x2: number;
      readonly y2: number;
      readonly stops: ReadonlyArray<{ readonly offset: number; readonly color: string }>;
    };
  }>;
}

function iconsTsxComponent(source: string, name: string): string {
  const start = source.indexOf(`export const ${name}: Icon`);
  if (start < 0) throw new Error(`Icons.tsx has no ${name}`);
  const end = source.indexOf("\nexport ", start + 1);
  return source.slice(start, end < 0 ? undefined : end);
}

function attribute(block: string, name: string): string | undefined {
  return new RegExp(`\\b${name}="([^"]*)"`).exec(block)?.[1];
}

function singlePathSvg(block: string, fill: string): string {
  const fillRule = attribute(block, "fillRule");
  const rule = fillRule ? ` fill-rule="${fillRule}" clip-rule="${fillRule}"` : "";
  return `<svg ${SVG_NS} viewBox="${attribute(block, "viewBox")}"><path${rule} fill="${fill}" d="${attribute(block, "d")}"/></svg>\n`;
}

function packageSvg(icon: PackageIcon, theme: "light" | "dark"): string {
  const defs = icon.paths
    .map((path, index) => {
      const gradient = path.gradient;
      if (!gradient) return "";
      const stops = gradient.stops
        .map((stop) => `<stop offset="${stop.offset}" stop-color="${stop.color}"/>`)
        .join("");
      return `<linearGradient id="g${index}" x1="${gradient.x1}" y1="${gradient.y1}" x2="${gradient.x2}" y2="${gradient.y2}" gradientUnits="userSpaceOnUse">${stops}</linearGradient>`;
    })
    .join("");
  const paths = icon.paths
    .map((path, index) => {
      const fill = path.gradient ? `url(#g${index})` : (path.fill ?? icon.fill)[theme];
      const rule = path.fillRule ? ` fill-rule="${path.fillRule}"` : "";
      return `<path${rule} fill="${fill}" d="${path.d}"/>`;
    })
    .join("");
  return `<svg ${SVG_NS} viewBox="${icon.viewBox}">${defs ? `<defs>${defs}</defs>` : ""}${paths}</svg>\n`;
}

/** File name to contents, for every provider mark `icons.rs` serves. */
async function providerFiles(): Promise<Map<string, Buffer>> {
  const files = new Map<string, Buffer>();
  const source = NodeFS.readFileSync(ICONS_TSX, "utf8");
  // Codex draws in the surrounding text color, so it ships as a one-color mask.
  files.set("codex.svg", Buffer.from(singlePathSvg(iconsTsxComponent(source, "OpenAI"), "#000")));
  const claude = iconsTsxComponent(source, "ClaudeAI");
  const claudeFill = /fill-\[(#[0-9a-fA-F]{6})\]/.exec(claude)?.[1];
  if (!claudeFill) throw new Error("ClaudeAI has no fill color");
  files.set("claudeAgent.svg", Buffer.from(singlePathSvg(claude, claudeFill)));
  const antigravity = /ANTIGRAVITY_ICON_DATA_URL =\s*"data:image\/png;base64,([^"]+)"/.exec(
    source,
  )?.[1];
  if (!antigravity) throw new Error("Icons.tsx has no Antigravity image");
  files.set("antigravity.png", Buffer.from(antigravity, "base64"));
  const { providerClients } = (await import(PROVIDER_CLIENTS)) as {
    providerClients: { definitions: ReadonlyArray<{ driverKind: string; icon?: PackageIcon }> };
  };
  // The ACP registry draws each agent's own icon from its registry entry instead.
  for (const client of providerClients.definitions) {
    if (!client.icon || client.driverKind === "acpRegistry") continue;
    for (const theme of ["light", "dark"] as const) {
      files.set(`${client.driverKind}-${theme}.svg`, Buffer.from(packageSvg(client.icon, theme)));
    }
  }
  return files;
}

const args = process.argv.slice(2);
NodeFS.mkdirSync(OUT, { recursive: true });
if (args[0] === "--check") {
  const missing = namesInIconsRs().filter(
    (name) => !NodeFS.existsSync(NodePath.join(OUT, `${name}.svg`)),
  );
  const stale = [...(await providerFiles())].filter(([name, contents]) => {
    const file = NodePath.join(PROVIDERS_OUT, name);
    return !NodeFS.existsSync(file) || !NodeFS.readFileSync(file).equals(contents);
  });
  if (missing.length > 0 || stale.length > 0) {
    console.error(
      `missing icons: ${[...missing, ...stale.map(([name]) => `providers/${name}`)].join(", ")}`,
    );
    process.exit(1);
  }
} else if (args[0] === "--providers") {
  NodeFS.mkdirSync(PROVIDERS_OUT, { recursive: true });
  for (const [name, contents] of await providerFiles()) {
    NodeFS.writeFileSync(NodePath.join(PROVIDERS_OUT, name), contents);
  }
} else {
  for (const name of args.length > 0 ? args : namesInIconsRs()) {
    const stroke = name.startsWith("stroke-2/") ? "2" : "1.75";
    NodeFS.mkdirSync(NodePath.dirname(NodePath.join(OUT, `${name}.svg`)), { recursive: true });
    NodeFS.writeFileSync(
      NodePath.join(OUT, `${name}.svg`),
      toSvg(lucideIcon(name.replace("stroke-2/", "")), stroke),
    );
  }
  NodeFS.copyFileSync(NodePath.join(LUCIDE, "LICENSE"), NodePath.join(OUT, "LICENSE"));
}

// Vendors the Lucide icons `crates/app/src/icons.rs` names, with the web's stroke baked in
// (design-system §8): 1.75, or 2 for files under `stroke-2/`.
// Usage: node apps/native/scripts/vendor-icons.ts [name | stroke-2/name]...   (default: all in icons.rs)
//        node apps/native/scripts/vendor-icons.ts --check                    (every icons.rs path has a file)
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
  return [...source.matchAll(/=> "icons\/lucide\/((?:stroke-2\/)?[a-z0-9-]+)\.svg"/g)].map((match) => match[1]!);
}

const args = process.argv.slice(2);
NodeFS.mkdirSync(OUT, { recursive: true });
if (args[0] === "--check") {
  const missing = namesInIconsRs().filter(
    (name) => !NodeFS.existsSync(NodePath.join(OUT, `${name}.svg`)),
  );
  if (missing.length > 0) {
    console.error(`missing icons: ${missing.join(", ")}`);
    process.exit(1);
  }
} else {
  for (const name of args.length > 0 ? args : namesInIconsRs()) {
    const stroke = name.startsWith("stroke-2/") ? "2" : "1.75";
    NodeFS.mkdirSync(NodePath.dirname(NodePath.join(OUT, `${name}.svg`)), { recursive: true });
    NodeFS.writeFileSync(NodePath.join(OUT, `${name}.svg`), toSvg(lucideIcon(name.replace("stroke-2/", "")), stroke));
  }
  NodeFS.copyFileSync(NodePath.join(LUCIDE, "LICENSE"), NodePath.join(OUT, "LICENSE"));
}

// Generates apps/native/crates/app/src/theme/tokens.rs from the web theme sources.
// Usage: node apps/native/scripts/gen-theme.ts [--check | --markdown]
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

type Appearance = "light" | "dark";
type Roles = Readonly<Record<string, string>>;
type ThemeDefinition = Readonly<{
  id: string;
  label: string;
  appearance: Appearance;
  colors: Roles;
  variants?: Readonly<Partial<Record<Appearance, Roles>>>;
}>;

const ROOT = NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "../../..");
const INDEX_CSS = "apps/web/src/index.css";
const WEB_PALETTE = "apps/web/src/themePalette.ts";
const SHARED_PALETTES = "packages/shared/src/themePalettes.ts";
const SETTINGS = "packages/contracts/src/settings.ts";
const OUTPUT = "apps/native/crates/app/src/theme/tokens.rs";

const read = (path: string) => NodeFS.readFileSync(NodePath.join(ROOT, path), "utf8");

type Decl = Readonly<{ prop: string; value: string; path: readonly string[] }>;

function parseCss(source: string): Decl[] {
  const text = source.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: Decl[] = [];
  let i = 0;
  const squash = (s: string) => s.replace(/\s+/g, " ").trim();

  function block(path: readonly string[]): void {
    let buf = "";
    let depth = 0;
    let quote = "";
    const flush = () => {
      const s = buf.trim();
      buf = "";
      if (!s || s.startsWith("@")) return;
      const colon = s.indexOf(":");
      if (colon <= 0) return;
      const value = squash(s.slice(colon + 1)).replace(/\s*!important$/, "");
      out.push({ prop: s.slice(0, colon).trim(), value, path });
    };
    while (i < text.length) {
      const ch = text[i++]!;
      if (quote) {
        buf += ch;
        if (ch === "\\") buf += text[i++] ?? "";
        else if (ch === quote) quote = "";
        continue;
      }
      if (ch === '"' || ch === "'") quote = ch;
      else if (ch === "(") depth++;
      else if (ch === ")") depth--;
      else if (depth === 0 && ch === "{") {
        const prelude = squash(buf);
        buf = "";
        block([...path, prelude]);
        continue;
      } else if (depth === 0 && ch === "}") {
        flush();
        return;
      } else if (depth === 0 && ch === ";") {
        flush();
        continue;
      }
      buf += ch;
    }
    flush();
  }

  block([]);
  return out;
}

const pathIs = (decl: Decl, ...path: string[]) =>
  decl.path.length === path.length && path.every((part, index) => decl.path[index] === part);

const require = NodeModule.createRequire(NodePath.join(ROOT, "apps/web/package.json"));
const tailwindThemePath = require.resolve("tailwindcss/theme.css");
const tailwindVersion = (
  JSON.parse(
    NodeFS.readFileSync(NodePath.join(NodePath.dirname(tailwindThemePath), "package.json"), "utf8"),
  ) as {
    version: string;
  }
).version;
const tailwindDecls = parseCss(NodeFS.readFileSync(tailwindThemePath, "utf8"));
const cssDecls = parseCss(read(INDEX_CSS));

const palettes = (await import(
  NodeURL.pathToFileURL(NodePath.join(ROOT, SHARED_PALETTES)).href
)) as {
  BUILT_IN_THEMES: readonly ThemeDefinition[];
  THEME_COLOR_ROLES: readonly string[];
};

function appThemeVariables(): ReadonlyMap<string, string> {
  const source = read(WEB_PALETTE);
  const start = source.indexOf("const APP_THEME_VARIABLES");
  const end = source.indexOf("};", start);
  if (start < 0 || end < 0) throw new Error(`APP_THEME_VARIABLES not found in ${WEB_PALETTE}`);
  const map = new Map<string, string>();
  for (const match of source.slice(start, end).matchAll(/(\w+): "(--app-theme-[a-z-]+)"/g)) {
    map.set(match[1]!, match[2]!);
  }
  for (const role of palettes.THEME_COLOR_ROLES) {
    if (!map.has(role))
      throw new Error(`Theme role "${role}" has no CSS variable in ${WEB_PALETTE}`);
  }
  return map;
}
const APP_THEME_VARIABLES = appThemeVariables();

function settingsFontSizes(): ReadonlyMap<string, number> {
  const map = new Map<string, number>();
  for (const match of read(SETTINGS).matchAll(
    /const ((?:DEFAULT|MIN|MAX)_(?:INTERFACE|PROMPT|CODE|TERMINAL)_FONT_SIZE)(?:: \w+)? = (\d+);/g,
  )) {
    map.set(match[1]!, Number(match[2]));
  }
  if (!map.has("DEFAULT_INTERFACE_FONT_SIZE")) {
    throw new Error(`DEFAULT_INTERFACE_FONT_SIZE not found in ${SETTINGS}`);
  }
  return map;
}
const FONT_SIZES = settingsFontSizes();
const ROOT_FONT_SIZE = FONT_SIZES.get("DEFAULT_INTERFACE_FONT_SIZE")!;

// Cascade. Custom properties resolve where they are declared: the root element,
// or the [data-app-sidebar] element, which inherits the root's computed values.
type Variant = Readonly<{ appearance: Appearance; theme: ThemeDefinition | null }>;
type Scope = Readonly<{ name: string; vars: ReadonlyMap<string, string>; parent: Scope | null }>;

const ROOT_AND_SIDEBAR = ":root, [data-app-sidebar]";
const THEMED_ROOT = 'html[data-theme-id]:not([data-theme-id=""])';
const THEMED_SIDEBAR = "html[data-theme-id] [data-app-sidebar]";

function declare(
  vars: Map<string, string>,
  decls: readonly Decl[],
  match: (decl: Decl) => boolean,
) {
  for (const decl of decls)
    if (decl.prop.startsWith("--") && match(decl)) vars.set(decl.prop, decl.value);
}

function themeRoles(variant: Variant): Roles | null {
  const { theme, appearance } = variant;
  if (!theme) return null;
  return theme.appearance === appearance ? theme.colors : (theme.variants?.[appearance] ?? null);
}

function rootScope(variant: Variant, extraPaths: readonly (readonly string[])[] = []): Scope {
  const vars = new Map<string, string>();
  const isTheme = (decl: Decl) => decl.path.length === 1 && decl.path[0]!.startsWith("@theme");
  declare(vars, tailwindDecls, isTheme);
  declare(vars, cssDecls, isTheme);
  declare(vars, cssDecls, (decl) => pathIs(decl, ":root") || pathIs(decl, ROOT_AND_SIDEBAR));
  if (variant.appearance === "dark")
    declare(vars, cssDecls, (decl) => pathIs(decl, ":root", "@variant dark"));
  const roles = themeRoles(variant);
  if (roles) {
    declare(vars, cssDecls, (decl) => pathIs(decl, THEMED_ROOT));
    for (const [role, value] of Object.entries(roles))
      vars.set(APP_THEME_VARIABLES.get(role)!, value);
  }
  for (const path of extraPaths) declare(vars, cssDecls, (decl) => pathIs(decl, ...path));
  return { name: "root", vars, parent: null };
}

function sidebarScope(variant: Variant, root: Scope): Scope {
  const vars = new Map<string, string>();
  declare(
    vars,
    cssDecls,
    (decl) => pathIs(decl, "[data-app-sidebar]") || pathIs(decl, ROOT_AND_SIDEBAR),
  );
  if (variant.appearance === "dark") {
    declare(vars, cssDecls, (decl) => pathIs(decl, "[data-app-sidebar]", "@variant dark"));
  }
  if (themeRoles(variant)) declare(vars, cssDecls, (decl) => pathIs(decl, THEMED_SIDEBAR));
  return { name: "sidebar", vars, parent: root };
}

function matchParen(text: string, open: number): number {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === "(") depth++;
    else if (text[i] === ")" && --depth === 0) return i;
  }
  throw new Error(`Unbalanced parentheses in "${text}"`);
}

function splitTop(text: string, separator: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    else if (depth === 0 && ch === separator) {
      parts.push(text.slice(start, i).trim());
      start = i + 1;
    }
  }
  parts.push(text.slice(start).trim());
  return parts;
}

function words(text: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let current = "";
  for (const ch of text) {
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (depth === 0 && (/\s/.test(ch) || ch === "/")) {
      if (current) out.push(current);
      if (ch === "/") out.push("/");
      current = "";
    } else {
      current += ch;
    }
  }
  if (current) out.push(current);
  return out;
}

function lookup(scope: Scope, name: string, seen: ReadonlySet<string>): string | undefined {
  for (let current: Scope | null = scope; current; current = current.parent) {
    const value = current.vars.get(name);
    if (value === undefined) continue;
    const key = `${current.name}:${name}`;
    if (seen.has(key)) throw new Error(`Cyclic custom property ${name}`);
    return substitute(current, value, new Set([...seen, key]));
  }
  return undefined;
}

function substitute(scope: Scope, text: string, seen: ReadonlySet<string> = new Set()): string {
  let out = "";
  let i = 0;
  for (;;) {
    const at = text.indexOf("var(", i);
    if (at < 0) return out + text.slice(i);
    out += text.slice(i, at);
    const end = matchParen(text, at + 3);
    const [name, ...rest] = splitTop(text.slice(at + 4, end), ",");
    const fallback = rest.length > 0 ? rest.join(",") : undefined;
    const value =
      lookup(scope, name!, seen) ??
      (fallback !== undefined ? substitute(scope, fallback, seen) : undefined);
    if (value === undefined) throw new Error(`Unresolved ${name} in scope ${scope.name}`);
    out += value;
    i = end + 1;
  }
}

// Color math. Colors are gamma-encoded sRGB, unclamped until output.
type Color = Readonly<{ r: number; g: number; b: number; a: number }>;
type Space = "srgb" | "oklab" | "oklch" | "lab";

const TRANSPARENT: Color = { r: 0, g: 0, b: 0, a: 0 };
const toLinear = (c: number) => {
  const x = Math.abs(c);
  return Math.sign(c) * (x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
};
const toGamma = (c: number) => {
  const x = Math.abs(c);
  return Math.sign(c) * (x <= 0.0031308 ? x * 12.92 : 1.055 * x ** (1 / 2.4) - 0.055);
};

function toOklab({ r, g, b }: Color): [number, number, number] {
  const [lr, lg, lb] = [toLinear(r), toLinear(g), toLinear(b)];
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function fromOklab(L: number, A: number, B: number, a: number): Color {
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.291485548 * B) ** 3;
  return {
    r: toGamma(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    g: toGamma(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    b: toGamma(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
    a,
  };
}

function fromOklch(L: number, C: number, h: number, a: number): Color {
  const rad = (h * Math.PI) / 180;
  return fromOklab(L, C * Math.cos(rad), C * Math.sin(rad), a);
}

// CIE Lab (D50), as CSS Color 4 defines it: Pierre's diff rows mix `in lab`.
type Matrix = readonly [readonly number[], readonly number[], readonly number[]];
const apply = (m: Matrix, [x, y, z]: readonly number[]): [number, number, number] => [
  m[0][0]! * x! + m[0][1]! * y! + m[0][2]! * z!,
  m[1][0]! * x! + m[1][1]! * y! + m[1][2]! * z!,
  m[2][0]! * x! + m[2][1]! * y! + m[2][2]! * z!,
];
const SRGB_TO_XYZ_D50: Matrix = [
  [0.4360747, 0.3850649, 0.1430804],
  [0.2225045, 0.7168786, 0.0606169],
  [0.0139322, 0.0971045, 0.7141733],
];
const XYZ_D50_TO_SRGB: Matrix = [
  [3.1338561, -1.6168667, -0.4906146],
  [-0.9787684, 1.9161415, 0.033454],
  [0.0719453, -0.2289914, 1.4052427],
];
const D50 = [0.3457 / 0.3585, 1, (1 - 0.3457 - 0.3585) / 0.3585] as const;
const LAB_E = 216 / 24389;
const LAB_K = 24389 / 27;

function toLab({ r, g, b }: Color): [number, number, number] {
  const xyz = apply(SRGB_TO_XYZ_D50, [toLinear(r), toLinear(g), toLinear(b)]);
  const [fx, fy, fz] = xyz.map((value, index) => {
    const v = value / D50[index]!;
    return v > LAB_E ? Math.cbrt(v) : (LAB_K * v + 16) / 116;
  }) as [number, number, number];
  return [116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)];
}

function fromLab(L: number, A: number, B: number, a: number): Color {
  const fy = (L + 16) / 116;
  const fx = A / 500 + fy;
  const fz = fy - B / 200;
  const inverse = (f: number) => (f ** 3 > LAB_E ? f ** 3 : (116 * f - 16) / LAB_K);
  const xyz = [
    inverse(fx) * D50[0],
    (L > LAB_K * LAB_E ? fy ** 3 : L / LAB_K) * D50[1],
    inverse(fz) * D50[2],
  ];
  const [lr, lg, lb] = apply(XYZ_D50_TO_SRGB, xyz);
  return { r: toGamma(lr), g: toGamma(lg), b: toGamma(lb), a };
}

function toSpace(color: Color, space: Space): [number, number, number] {
  if (space === "srgb") return [color.r, color.g, color.b];
  if (space === "lab") return toLab(color);
  const [L, A, B] = toOklab(color);
  if (space === "oklab") return [L, A, B];
  const C = Math.hypot(A, B);
  return [L, C, C < 1e-6 ? Number.NaN : ((Math.atan2(B, A) * 180) / Math.PI + 360) % 360];
}

function fromSpace([x, y, z]: [number, number, number], a: number, space: Space): Color {
  if (space === "srgb") return { r: x, g: y, b: z, a };
  if (space === "lab") return fromLab(x, y, z, a);
  return space === "oklab" ? fromOklab(x, y, z, a) : fromOklch(x, y, Number.isNaN(z) ? 0 : z, a);
}

// CSS Color 4 color-mix(): premultiplied interpolation, shorter hue arc.
function mix(space: Space, c1: Color, w1: number, c2: Color, w2: number): Color {
  const a = c1.a * w1 + c2.a * w2;
  if (a === 0) return TRANSPARENT;
  const x1 = toSpace(c1, space);
  const x2 = toSpace(c2, space);
  const out: [number, number, number] = [0, 0, 0];
  for (let k = 0; k < 3; k++) {
    if (space === "oklch" && k === 2) {
      let [h1, h2] = [x1[2], x2[2]];
      if (Number.isNaN(h1) && Number.isNaN(h2)) out[2] = Number.NaN;
      else {
        if (Number.isNaN(h1)) h1 = h2;
        if (Number.isNaN(h2)) h2 = h1;
        if (h2 - h1 > 180) h1 += 360;
        else if (h1 - h2 > 180) h2 += 360;
        out[2] = (h1 * w1 + h2 * w2) % 360;
      }
    } else {
      out[k] = (x1[k]! * c1.a * w1 + x2[k]! * c2.a * w2) / a;
    }
  }
  return fromSpace(out, a, space);
}

const isPercent = (word: string) => /^[+-]?(\d+\.?\d*|\.\d+)%$/.test(word);

function number(word: string): number {
  if (!/^[+-]?(\d+\.?\d*|\.\d+)(e[+-]?\d+)?$/i.test(word))
    throw new Error(`Not a number: "${word}"`);
  return Number(word);
}

const fraction = (word: string, scale = 1) =>
  word === "none" ? 0 : isPercent(word) ? (number(word.slice(0, -1)) / 100) * scale : number(word);

function channels(args: string): { values: string[]; alpha: number } {
  const list = args.includes(",") ? splitTop(args, ",") : words(args);
  const slash = list.indexOf("/");
  const values = slash < 0 ? list : list.slice(0, slash);
  const alphaWord = slash < 0 ? (args.includes(",") ? list[3] : undefined) : list[slash + 1];
  return { values: values.slice(0, 3), alpha: alphaWord === undefined ? 1 : fraction(alphaWord) };
}

function parseColor(input: string, appearance: Appearance): Color {
  const text = input.trim();
  const lower = text.toLowerCase();
  if (lower === "transparent") return TRANSPARENT;
  if (lower === "white") return { r: 1, g: 1, b: 1, a: 1 };
  if (lower === "black") return { r: 0, g: 0, b: 0, a: 1 };
  if (text.startsWith("#")) {
    let hex = text.slice(1);
    if (!/^([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(hex))
      throw new Error(`Bad hex "${text}"`);
    if (hex.length <= 4) hex = [...hex].map((c) => c + c).join("");
    const byte = (index: number) => parseInt(hex.slice(index * 2, index * 2 + 2), 16) / 255;
    return { r: byte(0), g: byte(1), b: byte(2), a: hex.length === 8 ? byte(3) : 1 };
  }
  const fn = /^(-{0,2}[a-z][a-z0-9-]*)\(/i.exec(text);
  if (!fn || matchParen(text, fn[1]!.length) !== text.length - 1) {
    throw new Error(`Unsupported color "${text}"`);
  }
  const args = text.slice(fn[1]!.length + 1, -1).trim();
  switch (fn[1]!.toLowerCase()) {
    case "rgb":
    case "rgba": {
      const { values, alpha } = channels(args);
      const [r, g, b] = values.map((v) => (isPercent(v) ? fraction(v) : number(v) / 255));
      return { r: r!, g: g!, b: b!, a: alpha };
    }
    case "oklch": {
      const { values, alpha } = channels(args);
      const hue = values[2] === "none" ? 0 : number(values[2]!.replace(/deg$/, ""));
      return fromOklch(fraction(values[0]!), fraction(values[1]!, 0.4), hue, alpha);
    }
    case "oklab": {
      const { values, alpha } = channels(args);
      return fromOklab(
        fraction(values[0]!),
        fraction(values[1]!, 0.4),
        fraction(values[2]!, 0.4),
        alpha,
      );
    }
    case "light-dark": {
      const [light, dark] = splitTop(args, ",");
      return parseColor(appearance === "dark" ? dark! : light!, appearance);
    }
    case "--alpha": {
      const [color, amount] = splitTop(args, "/");
      return parseColor(`color-mix(in oklab, ${color} ${amount}, transparent)`, appearance);
    }
    case "color-mix": {
      const [method, first, second] = splitTop(args, ",");
      const space = method!.replace(/^in\s+/, "").split(/\s+/)[0] as Space;
      if (
        !["srgb", "oklab", "oklch", "lab"].includes(space) ||
        /longer|increasing|decreasing/.test(method!)
      ) {
        throw new Error(`Unsupported color-mix method "${method}"`);
      }
      const operand = (part: string): [Color, number | undefined] => {
        const list = words(part);
        const pctIndex = isPercent(list[0]!) ? 0 : isPercent(list.at(-1)!) ? list.length - 1 : -1;
        const pct = pctIndex < 0 ? undefined : fraction(list[pctIndex]!);
        const color = list.filter((_, index) => index !== pctIndex).join(" ");
        return [parseColor(color, appearance), pct];
      };
      const [c1, p1Raw] = operand(first!);
      const [c2, p2Raw] = operand(second!);
      const p1 = p1Raw ?? (p2Raw === undefined ? 0.5 : 1 - p2Raw);
      const p2 = p2Raw ?? 1 - p1;
      const sum = p1 + p2;
      if (sum <= 0) throw new Error(`color-mix weights sum to zero in "${text}"`);
      const mixed = mix(space, c1, p1 / sum, c2, p2 / sum);
      return sum < 1 ? { ...mixed, a: mixed.a * sum } : mixed;
    }
    default:
      throw new Error(`Unsupported color function "${fn[1]}"`);
  }
}

function toU32(color: Color): number {
  const byte = (v: number) => Math.round(Math.min(1, Math.max(0, v)) * 255);
  return (
    ((byte(color.r) << 24) | (byte(color.g) << 16) | (byte(color.b) << 8) | byte(color.a)) >>> 0
  );
}

const hex32 = (value: number) => `0x${value.toString(16).toUpperCase().padStart(8, "0")}`;

// Tokens. `css` is evaluated in its scope; rule-backed tokens read a property
// from the rule (and its `@variant dark` child) that paints it on the web.
type TokenContext = Readonly<{
  variant: Variant;
  rule: (path: readonly string[], prop: string) => string | undefined;
}>;
type ColorToken = Readonly<{
  name: string;
  web: string;
  scope?: "sidebar";
  css: string | ((context: TokenContext) => string);
}>;

const v = (name: string) => `var(${name})`;
const CODE_BLOCK_THEMED =
  "html[data-theme-id] :is(.chat-markdown, .composer-tiptap) .chat-markdown-codeblock";

// Status hues the web hard-codes as Tailwind classes instead of theme variables
// (Sidebar.logic.ts resolveThreadStatusPill, SidebarThreadRow.tsx, ThreadStatusIndicators.tsx).
// `/90` compiles to color-mix(in oklab, ... 90%, transparent). Re-transcribe if those classes change.
const hue = (light: string, dark: string, darkAlpha = 100) =>
  `light-dark(var(--color-${light}), color-mix(in oklab, var(--color-${dark}) ${darkAlpha}%, transparent))`;
const statusHue = (name: string, color: string, darkAlpha: number): ColorToken[] => [
  {
    name: `pill_${name}`,
    web: `text-${color}-600 dark:text-${color}-300/${darkAlpha}`,
    css: hue(`${color}-600`, `${color}-300`, darkAlpha),
  },
  {
    name: `pill_${name}_dot`,
    web: `bg-${color}-500 dark:bg-${color}-300/${darkAlpha}`,
    css: hue(`${color}-500`, `${color}-300`, darkAlpha),
  },
];

// Usage page hues, read from the web source so a changed color shows as drift: each provider's
// series color (`usageProviders.ts` PROVIDER_PRESENTATION) and the share-bar inks
// (`usageBreakdown.ts` `ink(percent)`, foreground mixed into the background).
const USAGE_PROVIDERS = "apps/web/src/components/usage/usageProviders.ts";
const USAGE_BREAKDOWN = "apps/web/src/components/usage/usageBreakdown.ts";
const usageTokens = (): ColorToken[] => {
  const providers = [
    ...read(USAGE_PROVIDERS).matchAll(/(\w+):\s*\{\s*label:\s*"[^"]*",\s*color:\s*"([^"]+)"/g),
  ];
  const inks = [
    ...new Set(
      [...read(USAGE_BREAKDOWN).matchAll(/ink\((\d+)\)/g)].map((match) => Number(match[1])),
    ),
  ].sort((a, b) => a - b);
  if (providers.length === 0 || inks.length === 0) throw new Error("usage colors not found");
  return [
    ...providers.map(([, id, color]) => ({
      name: `usage_${id}`,
      web: `usageProviders.ts ${id}`,
      css: color!,
    })),
    ...inks.map((percent) => ({
      name: `usage_ink_${percent}`,
      web: `usageBreakdown.ts ink(${percent})`,
      css: `color-mix(in oklab, var(--foreground) ${percent}%, var(--background))`,
    })),
  ];
};

// What a changed diff row paints: Pierre mixes its line color `in lab` over the code surface
// (lines 88%/80%, gutter 91%/85%) toward the override `diffRendering.ts` sets
// (`--diffs-bg-{kind}-override` and `--diffs-bg-{kind}-number-override`). Inside the viewer
// `StyledDiffCodeView.tsx` sets `--code-background: var(--background)`, so the surface is the canvas.
const diffRowTokens = (kind: "addition" | "deletion"): ColorToken[] => {
  const row = (line: [number, number], target: [number, number]) =>
    `light-dark(${[0, 1]
      .map(
        (i) =>
          `color-mix(in lab, var(--background) ${line[i]}%, color-mix(in srgb, var(--background) ${target[i]}%, var(--diff-${kind})))`,
      )
      .join(", ")})`;
  return [
    {
      name: `diff_row_${kind}`,
      web: `diffRendering.ts --diffs-bg-${kind}-override`,
      css: row([88, 80], [50, 70]),
    },
    {
      name: `diff_row_${kind}_gutter`,
      web: `diffRendering.ts --diffs-bg-${kind}-number-override`,
      css: row([91, 85], [35, 60]),
    },
  ];
};

const COLOR_TOKENS: readonly ColorToken[] = [
  {
    name: "app_chrome_background",
    web: "--app-chrome-background",
    css: v("--app-chrome-background"),
  },
  { name: "background", web: "--background", css: v("--background") },
  { name: "foreground", web: "--color-foreground", css: v("--contrast-foreground") },
  { name: "surface_raised", web: "--surface-raised", css: v("--surface-raised") },
  { name: "card", web: "--card", css: v("--card") },
  { name: "card_foreground", web: "--color-card-foreground", css: v("--contrast-card-foreground") },
  { name: "popover", web: "--popover", css: v("--popover") },
  {
    name: "popover_foreground",
    web: "--color-popover-foreground",
    css: v("--contrast-popover-foreground"),
  },
  { name: "primary", web: "--primary", css: v("--primary") },
  { name: "primary_foreground", web: "--primary-foreground", css: v("--primary-foreground") },
  { name: "secondary", web: "--secondary", css: v("--secondary") },
  {
    name: "secondary_foreground",
    web: "--color-secondary-foreground",
    css: v("--contrast-secondary-foreground"),
  },
  { name: "muted", web: "--muted", css: v("--muted") },
  {
    name: "muted_foreground",
    web: "--color-muted-foreground",
    css: v("--contrast-muted-foreground"),
  },
  { name: "accent", web: "--accent", css: v("--accent") },
  {
    name: "accent_foreground",
    web: "--color-accent-foreground",
    css: v("--contrast-accent-foreground"),
  },
  { name: "placeholder", web: "--color-placeholder", css: v("--contrast-placeholder") },
  { name: "secondary_label", web: "--color-secondary-label", css: v("--contrast-secondary-label") },
  { name: "icon_muted", web: "--color-icon-muted", css: v("--contrast-icon-muted") },
  { name: "border", web: "--color-border", css: v("--contrast-border") },
  { name: "input", web: "--color-input", css: v("--contrast-input") },
  { name: "ring", web: "--ring", css: v("--ring") },
  { name: "destructive", web: "--destructive", css: v("--destructive") },
  {
    name: "destructive_foreground",
    web: "--destructive-foreground",
    css: v("--destructive-foreground"),
  },
  { name: "error", web: "--error", css: v("--error") },
  { name: "error_foreground", web: "--error-foreground", css: v("--error-foreground") },
  { name: "error_surface", web: "--error-surface", css: v("--error-surface") },
  { name: "tool_error_icon", web: "--tool-error-icon", css: v("--tool-error-icon") },
  { name: "warning", web: "--warning", css: v("--warning") },
  { name: "warning_foreground", web: "--warning-foreground", css: v("--warning-foreground") },
  { name: "warning_surface", web: "--warning-surface", css: v("--warning-surface") },
  { name: "success", web: "--success", css: v("--success") },
  { name: "success_foreground", web: "--success-foreground", css: v("--success-foreground") },
  { name: "info", web: "--info", css: v("--info") },
  { name: "info_foreground", web: "--info-foreground", css: v("--info-foreground") },
  { name: "update", web: "--update", css: v("--update") },
  { name: "update_foreground", web: "--update-foreground", css: v("--update-foreground") },
  { name: "guest", web: "--guest", css: v("--guest") },
  { name: "guest_foreground", web: "--guest-foreground", css: v("--guest-foreground") },
  { name: "guest_surface", web: "--guest-surface", css: v("--guest-surface") },
  { name: "message_surface", web: "--message-surface", css: v("--message-surface") },
  {
    name: "message_foreground",
    web: "--color-message-foreground",
    css: v("--contrast-message-foreground"),
  },
  { name: "message_action", web: "--message-action", css: v("--message-action") },
  {
    name: "message_action_foreground",
    web: "--message-action-foreground",
    css: v("--message-action-foreground"),
  },
  { name: "message_action_hover", web: "--message-action-hover", css: v("--message-action-hover") },
  { name: "code_background", web: "--code-background", css: v("--code-background") },
  { name: "code_foreground", web: "--code-foreground", css: v("--code-foreground") },
  // MarkdownCodeBlock.tsx: `border-border/70 bg-secondary dark:border-transparent dark:bg-input/32`;
  // named themes override it with an unlayered index.css rule.
  {
    name: "code_block_background",
    web: ".chat-markdown-codeblock",
    css: ({ variant, rule }) =>
      themeRoles(variant)
        ? rule([CODE_BLOCK_THEMED], "background-color")!
        : "light-dark(var(--secondary), color-mix(in oklab, var(--contrast-input) 32%, transparent))",
  },
  {
    name: "code_block_border",
    web: ".chat-markdown-codeblock",
    css: ({ variant, rule }) =>
      themeRoles(variant)
        ? rule([CODE_BLOCK_THEMED], "border-color")!
        : "light-dark(color-mix(in oklab, var(--contrast-border) 70%, transparent), transparent)",
  },
  {
    name: "inline_code_background",
    web: ".chat-markdown :not(pre) > code",
    css: ({ variant, rule }) =>
      (variant.theme?.id === "t3-chat" && variant.appearance === "light"
        ? rule(
            [
              'html[data-theme-id="t3-chat"]',
              "@variant light",
              "& .chat-markdown :not(pre) > code, & [data-changed-files-state], & [data-changed-files-header]",
            ],
            "background-color",
          )
        : undefined) ?? rule([".chat-markdown :not(pre) > code"], "background")!,
  },
  { name: "diff_addition", web: "--diff-addition", css: v("--diff-addition") },
  { name: "diff_deletion", web: "--diff-deletion", css: v("--diff-deletion") },
  {
    name: "diff_addition_foreground",
    web: "--diff-addition-foreground",
    css: v("--diff-addition-foreground"),
  },
  {
    name: "diff_deletion_foreground",
    web: "--diff-deletion-foreground",
    css: v("--diff-deletion-foreground"),
  },
  { name: "diff_modified_icon", web: "--diff-modified-icon", css: v("--diff-modified-icon") },
  ...diffRowTokens("addition"),
  ...diffRowTokens("deletion"),
  {
    name: "diff_hunk_separator",
    web: "StyledDiffCodeView.tsx --diffs-bg-separator",
    css: "color-mix(in srgb, var(--background) 97%, var(--code-foreground))",
  },
  { name: "toolbar_background", web: "--toolbar-background", css: v("--toolbar-background") },
  {
    name: "toolbar_foreground",
    web: "--contrast-toolbar-foreground",
    css: v("--contrast-toolbar-foreground"),
  },
  { name: "toolbar_border", web: "--contrast-toolbar-border", css: v("--contrast-toolbar-border") },
  { name: "toolbar_control", web: "--toolbar-control", css: v("--toolbar-control") },
  {
    name: "toolbar_control_foreground",
    web: "--contrast-toolbar-control-foreground",
    css: v("--contrast-toolbar-control-foreground"),
  },
  {
    name: "toolbar_control_hover",
    web: "--toolbar-control-hover",
    css: v("--toolbar-control-hover"),
  },
  { name: "terminal_background", web: "--terminal-background", css: v("--terminal-background") },
  { name: "terminal_foreground", web: "--terminal-foreground", css: v("--terminal-foreground") },
  { name: "terminal_cursor", web: "--terminal-cursor", css: v("--terminal-cursor") },
  {
    name: "terminal_selection",
    web: "--terminal-selection-background",
    css: v("--terminal-selection-background"),
  },
  { name: "scrollbar_thumb", web: "--app-scrollbar-thumb", css: v("--app-scrollbar-thumb") },
  {
    name: "scrollbar_thumb_hover",
    web: "--app-scrollbar-thumb-hover",
    css: v("--app-scrollbar-thumb-hover"),
  },
  {
    name: "search_match_background",
    web: "::highlight(t3-thread-find)",
    css: ({ rule }) => rule(["::highlight(t3-thread-find)"], "background-color")!,
  },
  {
    name: "search_match_foreground",
    web: "::highlight(t3-thread-find)",
    css: ({ rule }) => rule(["::highlight(t3-thread-find)"], "color")!,
  },
  {
    name: "search_match_active_background",
    web: "::highlight(t3-thread-find-active)",
    css: ({ rule }) => rule(["::highlight(t3-thread-find-active)"], "background-color")!,
  },
  {
    name: "search_match_active_foreground",
    web: "::highlight(t3-thread-find-active)",
    css: ({ rule }) => rule(["::highlight(t3-thread-find-active)"], "color")!,
  },
  {
    name: "status_input_icon",
    web: "text-indigo-600 dark:text-indigo-300",
    css: hue("indigo-600", "indigo-300"),
  },
  {
    name: "terminal_running",
    web: "text-teal-600 dark:text-teal-300/90",
    css: hue("teal-600", "teal-300", 90),
  },
  ...statusHue("approval", "amber", 90),
  ...statusHue("input", "indigo", 90),
  ...statusHue("working", "sky", 80),
  ...statusHue("failed", "red", 90),
  ...statusHue("plan", "violet", 90),
  ...statusHue("completed", "emerald", 90),
  ...usageTokens(),
  {
    name: "glass_surface",
    web: "@utility surface-glass",
    css: ({ rule }) => rule(["@utility surface-glass"], "background")!,
  },
  {
    name: "dialog_surface",
    web: "@utility dialog-glass",
    css: ({ rule }) => rule(["@utility dialog-glass"], "background")!,
  },
  {
    name: "dialog_border",
    web: "@utility dialog-glass",
    css: ({ rule }) => rule(["@utility dialog-glass"], "border-color")!,
  },
  {
    name: "dialog_backdrop",
    web: "@utility dialog-backdrop",
    css: ({ rule }) => rule(["@utility dialog-backdrop"], "background")!,
  },
  {
    name: "dropdown_surface",
    web: "@utility dropdown-glass",
    css: ({ rule }) => rule(["@utility dropdown-glass"], "background")!,
  },
  {
    name: "dropdown_border",
    web: "@utility dropdown-glass",
    css: ({ rule }) => rule(["@utility dropdown-glass"], "border")!.replace(/^1px solid /, ""),
  },
  { name: "sidebar", web: "--sidebar", scope: "sidebar", css: v("--sidebar") },
  {
    name: "sidebar_foreground",
    web: "--color-sidebar-foreground",
    scope: "sidebar",
    css: v("--contrast-sidebar-foreground"),
  },
  {
    name: "sidebar_muted_foreground",
    web: "--color-sidebar-muted-foreground",
    scope: "sidebar",
    css: v("--contrast-sidebar-muted-foreground"),
  },
  {
    name: "sidebar_icon",
    web: "--sidebar-icon-color",
    scope: "sidebar",
    css: v("--sidebar-icon-color"),
  },
  {
    name: "sidebar_secondary_label",
    web: "--color-secondary-label",
    scope: "sidebar",
    css: v("--contrast-secondary-label"),
  },
  { name: "sidebar_accent", web: "--accent", scope: "sidebar", css: v("--accent") },
  {
    name: "sidebar_control_surface",
    web: "--sidebar-control-surface",
    scope: "sidebar",
    css: v("--sidebar-control-surface"),
  },
  {
    name: "sidebar_row_hover",
    web: "--sidebar-row-hover",
    scope: "sidebar",
    css: v("--sidebar-row-hover"),
  },
  {
    name: "sidebar_row_active",
    web: "--sidebar-row-active",
    scope: "sidebar",
    css: v("--sidebar-row-active"),
  },
  {
    name: "sidebar_row_selected",
    web: "--sidebar-row-selected",
    scope: "sidebar",
    css: v("--sidebar-row-selected"),
  },
  {
    name: "sidebar_border",
    web: "--color-sidebar-border",
    scope: "sidebar",
    css: v("--contrast-sidebar-border"),
  },
  {
    name: "sidebar_edge",
    web: "[data-app-sidebar] border-color",
    scope: "sidebar",
    css: ({ variant }) => {
      if (!themeRoles(variant)) return v("--contrast-border");
      // Ascending specificity: generic, theme-specific, then their dark variants.
      const specific = `html[data-theme-id="${variant.theme!.id}"] [data-app-sidebar]`;
      const candidates: (readonly string[])[] = [[THEMED_SIDEBAR], [specific]];
      if (variant.appearance === "dark")
        candidates.push([THEMED_SIDEBAR, "@variant dark"], [specific, "@variant dark"]);
      let value: string | undefined;
      for (const path of candidates) value = exactRule(path, "border-color") ?? value;
      return value!;
    },
  },
];

function exactRule(path: readonly string[], prop: string): string | undefined {
  let value: string | undefined;
  for (const decl of cssDecls) if (decl.prop === prop && pathIs(decl, ...path)) value = decl.value;
  return value;
}

// A rule's `@variant dark` child outranks the rule itself, whatever the source order.
function ruleValue(variant: Variant) {
  return (path: readonly string[], prop: string) =>
    (variant.appearance === "dark" ? exactRule([...path, "@variant dark"], prop) : undefined) ??
    exactRule(path, prop);
}

type Palette = ReadonlyMap<string, number>;

function palette(variant: Variant): Palette {
  const root = rootScope(variant);
  const sidebar = sidebarScope(variant, root);
  const context: TokenContext = { variant, rule: ruleValue(variant) };
  const out = new Map<string, number>();
  for (const token of COLOR_TOKENS) {
    const css = typeof token.css === "string" ? token.css : token.css(context);
    if (!css) throw new Error(`Token ${token.name} has no source value`);
    const scope = token.scope === "sidebar" ? sidebar : root;
    try {
      out.set(token.name, toU32(parseColor(substitute(scope, css), variant.appearance)));
    } catch (cause) {
      const theme = variant.theme ? `${variant.theme.id} ` : "";
      throw new Error(
        `Token ${token.name} (${theme}${variant.appearance}): ${(cause as Error).message}`,
        { cause },
      );
    }
  }
  return out;
}

// Scalars. Lengths resolve to px at the default interface font size; values
// that involve rem scale with Settings -> Appearance -> Interface font size.
type Scalar = Readonly<{ value: number; unit: string; rem: boolean }>;

function evalScalar(text: string): Scalar {
  const tokens = text.match(/calc\(|max\(|min\(|[+-]?(?:\d+\.?\d*|\.\d+)[a-z%]*|[-+*/()]/gi) ?? [];
  let index = 0;
  const peek = () => tokens[index];
  const next = () => tokens[index++];

  function atom(): Scalar {
    const token = next();
    if (token === undefined) throw new Error(`Unexpected end of "${text}"`);
    if (/^calc\($|^\($/i.test(token)) {
      const value = sum();
      if (next() !== ")") throw new Error(`Expected ")" in "${text}"`);
      return value;
    }
    const match = /^([+-]?(?:\d+\.?\d*|\.\d+))([a-z%]*)$/i.exec(token);
    if (!match) throw new Error(`Unsupported scalar "${text}"`);
    const amount = Number(match[1]);
    const unit = match[2]!.toLowerCase();
    if (unit === "rem") return { value: amount * ROOT_FONT_SIZE, unit: "px", rem: true };
    return { value: amount, unit, rem: false };
  }

  function product(): Scalar {
    let left = atom();
    while (peek() === "*" || peek() === "/") {
      const op = next();
      const right = atom();
      if (right.unit && left.unit) throw new Error(`Cannot ${op} two lengths in "${text}"`);
      left = {
        value: op === "*" ? left.value * right.value : left.value / right.value,
        unit: left.unit || right.unit,
        rem: left.rem || right.rem,
      };
    }
    return left;
  }

  function sum(): Scalar {
    let left = product();
    while (peek() === "+" || peek() === "-") {
      const op = next();
      const right = product();
      if (left.unit !== right.unit) throw new Error(`Mismatched units in "${text}"`);
      left = {
        value: op === "+" ? left.value + right.value : left.value - right.value,
        unit: left.unit,
        rem: left.rem || right.rem,
      };
    }
    return left;
  }

  const result = sum();
  if (index !== tokens.length) throw new Error(`Trailing input in "${text}"`);
  return result;
}

type Metric = Readonly<{ name: string; value: number; note: string }>;

function metrics(): Metric[] {
  const light = rootScope({ appearance: "light", theme: null });
  const dark = rootScope({ appearance: "dark", theme: null });
  const out: Metric[] = [];
  const raw = (scope: Scope, name: string) => {
    const value = scope.vars.get(name);
    if (value === undefined) throw new Error(`Metric source ${name} not found`);
    return value;
  };
  const add = (
    name: string,
    cssVar: string,
    scope: Scope = light,
    transform = (n: number) => n,
  ) => {
    const declared = raw(scope, cssVar);
    const scalar = evalScalar(substitute(scope, declared));
    out.push({
      name,
      value: transform(scalar.value),
      note: `${scalar.rem ? "rem" : scalar.unit || "number"}: ${cssVar}`,
    });
  };

  out.push({
    name: "ROOT_FONT_SIZE",
    value: ROOT_FONT_SIZE,
    note: "px: DEFAULT_INTERFACE_FONT_SIZE",
  });
  for (const [name, value] of FONT_SIZES) {
    if (name !== "DEFAULT_INTERFACE_FONT_SIZE") out.push({ name, value, note: `px: ${name}` });
  }
  add("SPACING", "--spacing");
  add("RADIUS", "--radius");
  for (const step of ["xs", "sm", "md", "lg", "xl", "2xl", "3xl", "4xl"]) {
    add(`RADIUS_${step.toUpperCase()}`, `--radius-${step}`);
  }
  add("CONTROL_RADIUS", "--control-radius");

  const textSteps = [...light.vars.keys()]
    .map((name) => /^--text-([0-9a-z]+)$/.exec(name)?.[1])
    .filter(
      (step): step is string => step !== undefined && light.vars.has(`--text-${step}--line-height`),
    );
  const sizeOf = (step: string) =>
    evalScalar(substitute(light, raw(light, `--text-${step}`))).value;
  textSteps.sort((a, b) => sizeOf(a) - sizeOf(b));
  for (const step of textSteps) {
    const size = sizeOf(step);
    add(`TEXT_${step.toUpperCase()}`, `--text-${step}`);
    add(
      `TEXT_${step.toUpperCase()}_LINE_HEIGHT`,
      `--text-${step}--line-height`,
      light,
      (ratio) => ratio * size,
    );
    out.at(-1)!.note = `rem: --text-${step} * --text-${step}--line-height`;
  }
  for (const group of ["font-weight", "tracking", "leading"]) {
    const prefix = `--${group}-`;
    for (const name of light.vars.keys()) {
      if (name.startsWith(prefix))
        add(
          `${group.replace("-", "_").toUpperCase()}_${name.slice(prefix.length).toUpperCase()}`,
          name,
        );
    }
  }

  const layout: readonly (readonly [string, string])[] = [
    ["SIDEBAR_CONTENT_INSET", "--sidebar-content-inset"],
    ["SIDEBAR_CONTROL_GAP", "--sidebar-control-gap"],
    ["SIDEBAR_ROW_CONTENT_INSET", "--sidebar-row-content-inset"],
    ["COMMAND_SHELL_INSET", "--command-shell-inset"],
    ["COMMAND_CONTENT_INSET", "--command-content-inset"],
    ["FLOATING_CONTENT_INSET", "--floating-content-inset"],
    ["WORKSPACE_TOPBAR_HEIGHT", "--workspace-topbar-height"],
    ["WORKSPACE_TITLEBAR_CONTROL_SIZE", "--workspace-titlebar-control-size"],
    ["WORKSPACE_TITLEBAR_CONTROL_GAP", "--workspace-titlebar-control-gap"],
    ["WORKSPACE_TITLEBAR_SCROLL_FADE_HEIGHT", "--workspace-titlebar-scroll-fade-height"],
    ["WORKSPACE_GUTTER", "--workspace-gutter"],
    ["CHAT_CONTENT_MAX_WIDTH", "--chat-content-max-width"],
    ["THREAD_DETAILS_PANEL_WIDTH", "--thread-details-panel-width"],
    ["APP_SCROLLBAR_WIDTH", "--app-scrollbar-width"],
    ["Z_SHEET", "--z-sheet"],
    ["GLASS_BLUR", "--glass-blur"],
    ["GLASS_SATURATION", "--glass-saturation"],
    ["DEFAULT_TRANSITION_DURATION_MS", "--default-transition-duration"],
  ];
  for (const [name, cssVar] of layout) add(name, cssVar);
  add("GLASS_OPACITY", "--glass-opacity", light, (pct) => pct / 100);
  add("GLASS_BLUR_DARK", "--glass-blur", dark);
  add("GLASS_SATURATION_DARK", "--glass-saturation", dark);
  add(
    "WORKSPACE_GUTTER_SM",
    "--workspace-gutter",
    rootScope({ appearance: "light", theme: null }, [[":root", "@variant sm"]]),
  );
  add(
    "CHAT_CONTENT_MAX_WIDTH_WIDE",
    "--chat-content-max-width",
    rootScope({ appearance: "light", theme: null }, [[':root[data-chat-width="wide"]']]),
  );
  return out;
}

function easings(): (readonly [string, readonly number[], string])[] {
  const light = rootScope({ appearance: "light", theme: null });
  const out: (readonly [string, readonly number[], string])[] = [];
  const add = (name: string, cssVar: string) => {
    const value = substitute(light, light.vars.get(cssVar) ?? "");
    const match = /^cubic-bezier\(([^)]*)\)$/.exec(value);
    if (!match) throw new Error(`Easing ${cssVar} is not a cubic-bezier: "${value}"`);
    out.push([name, match[1]!.split(",").map((n) => number(n.trim())), cssVar]);
  };
  add("EASE_IN", "--ease-in");
  add("EASE_OUT", "--ease-out");
  add("EASE_IN_OUT", "--ease-in-out");
  add("EASE_DRAWER", "--ease-drawer");
  add("DEFAULT_TRANSITION_EASE", "--default-transition-timing-function");
  return out;
}

// Syntax colors: the web highlights code with tree-sitter through `@pierre/highlights`, whose Zed
// palettes map 1:1 onto gpui-component's highlight theme. `lib/diffRendering.ts` picks
// pierre-dark, and t3-light (`lib/readableSyntaxTheme.ts`): pierre-light darkened to read on
// every light code surface.
const HIGHLIGHTS_PACKAGE = NodeModule.createRequire(
  NodePath.join(
    NodeFS.realpathSync(NodePath.join(ROOT, "apps/web/node_modules/@pierre/diffs")),
    "package.json",
  ),
).resolve("@pierre/highlights/package.json");
const highlightsVersion = (
  JSON.parse(NodeFS.readFileSync(HIGHLIGHTS_PACKAGE, "utf8")) as { version: string }
).version;

type ZedStyle = Readonly<{ color?: string; font_style?: string; font_weight?: number }>;

// `readableSyntaxTheme.ts`: the darkest light code surface, the contrast floor, and the colors
// pushed further so pairs that differ only in lightness stay apart.
const READABLE_MIN_CONTRAST = 4.5;
const READABLE_DARKEST_SURFACE_LUMINANCE = 0.69;
const READABLE_FURTHER_COLORS = new Set(["#1ca1c7", "#636363", "#1a85d4"]);
const READABLE_FURTHER_MIN_CONTRAST = 6;

function readableSyntaxColor(color: string): string {
  const match = /^#([0-9a-f]{6})([0-9a-f]{2})?$/i.exec(color);
  if (!match) return color;
  const minContrast = READABLE_FURTHER_COLORS.has(color.toLowerCase())
    ? READABLE_FURTHER_MIN_CONTRAST
    : READABLE_MIN_CONTRAST;
  const luminance = (channel: number) => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const contrast = ([r, g, b]: number[]) =>
    (READABLE_DARKEST_SURFACE_LUMINANCE + 0.05) /
    (0.2126 * luminance(r!) + 0.7152 * luminance(g!) + 0.0722 * luminance(b!) + 0.05);
  let rgb = [0, 2, 4].map((start) => Number.parseInt(match[1]!.slice(start, start + 2), 16));
  if (contrast(rgb) >= minContrast) return color;
  while (contrast(rgb) < minContrast) rgb = rgb.map((channel) => Math.floor(channel * 0.96));
  return `#${rgb.map((channel) => channel.toString(16).padStart(2, "0")).join("")}${match[2] ?? ""}`;
}

async function syntaxPalette(
  name: string,
  readable: boolean,
): Promise<(readonly [string, ZedStyle])[]> {
  const file = NodePath.join(NodePath.dirname(HIGHLIGHTS_PACKAGE), "dist/themes", `${name}.js`);
  const theme = (await import(NodeURL.pathToFileURL(file).href)) as {
    default: { style: { syntax: Record<string, ZedStyle> } };
  };
  return Object.entries(theme.default.style.syntax)
    .map(
      ([capture, style]) =>
        [
          capture,
          readable && style.color ? { ...style, color: readableSyntaxColor(style.color) } : style,
        ] as const,
    )
    .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
}

const SYNTAX = [
  ["SYNTAX_LIGHT", "t3-light", await syntaxPalette("pierre-light", true)],
  ["SYNTAX_DARK", "pierre-dark", await syntaxPalette("pierre-dark", false)],
] as const;

function syntaxColor(color: string | undefined): string {
  if (color === undefined) return "None";
  const match = /^#([0-9a-f]{6})([0-9a-f]{2})?$/i.exec(color);
  if (!match) throw new Error(`Unsupported syntax color "${color}"`);
  return `Some(0x${match[1]!.toUpperCase()}${(match[2] ?? "ff").toUpperCase()})`;
}

function f32(value: number): string {
  const rounded = Number(value.toFixed(4));
  const text = String(Object.is(rounded, -0) ? 0 : rounded);
  return /[.e]/.test(text) ? text : `${text}.0`;
}

const constName = (id: string) => id.replace(/[^a-z0-9]+/gi, "_").toUpperCase();

type Variants = readonly (readonly [constName: string, variant: Variant])[];

function variants(): Variants {
  const out: (readonly [string, Variant])[] = [
    ["LIGHT", { appearance: "light", theme: null }],
    ["DARK", { appearance: "dark", theme: null }],
  ];
  for (const theme of palettes.BUILT_IN_THEMES) {
    for (const appearance of ["light", "dark"] as const) {
      if (themeRoles({ appearance, theme })) {
        out.push([`${constName(theme.id)}_${appearance.toUpperCase()}`, { appearance, theme }]);
      }
    }
  }
  return out;
}

function renderRust(): string {
  const lines: string[] = [];
  const sources = [
    INDEX_CSS,
    WEB_PALETTE,
    SHARED_PALETTES,
    SETTINGS,
    USAGE_PROVIDERS,
    USAGE_BREAKDOWN,
    `tailwindcss@${tailwindVersion}/theme.css`,
    `@pierre/highlights@${highlightsVersion}`,
  ];
  lines.push(
    `// @generated by apps/native/scripts/gen-theme.ts from ${sources.join(", ")}. Do not edit.`,
  );
  lines.push("#![allow(dead_code)]", "");
  lines.push("#[derive(Clone, Copy, Debug)]", "pub struct Palette {");
  for (const token of COLOR_TOKENS) lines.push(`    pub ${token.name}: u32,`);
  lines.push("}", "");

  const all = variants();
  for (const [name, variant] of all) {
    const values = palette(variant);
    lines.push(`pub const ${name}: Palette = Palette {`);
    for (const token of COLOR_TOKENS)
      lines.push(`    ${token.name}: ${hex32(values.get(token.name)!)},`);
    lines.push("};", "");
  }

  lines.push("#[derive(Clone, Copy, Debug)]", "pub struct BuiltInTheme {");
  lines.push("    pub id: &'static str,", "    pub label: &'static str,");
  lines.push("    pub light: Option<Palette>,", "    pub dark: Option<Palette>,", "}", "");
  lines.push("pub const BUILT_IN_THEMES: &[BuiltInTheme] = &[");
  for (const theme of palettes.BUILT_IN_THEMES) {
    const variantName = (appearance: Appearance) =>
      themeRoles({ appearance, theme })
        ? `Some(${constName(theme.id)}_${appearance.toUpperCase()})`
        : "None";
    lines.push("    BuiltInTheme {");
    lines.push(
      `        id: ${JSON.stringify(theme.id)},`,
      `        label: ${JSON.stringify(theme.label)},`,
    );
    lines.push(
      `        light: ${variantName("light")},`,
      `        dark: ${variantName("dark")},`,
      "    },",
    );
  }
  lines.push("];", "");

  lines.push(
    "/// One highlight capture's style in a syntax palette.",
    "#[derive(Clone, Copy, Debug)]",
  );
  lines.push("pub struct SyntaxStyle {");
  lines.push(
    "    pub capture: &'static str,",
    "    pub color: Option<u32>,",
    "    pub italic: bool,",
    "    pub bold: bool,",
    "}",
    "",
  );
  for (const [name, source, styles] of SYNTAX) {
    lines.push(
      `/// \`${source}\` by tree-sitter capture.`,
      `pub const ${name}: &[SyntaxStyle] = &[`,
    );
    for (const [capture, style] of styles) {
      const italic = style.font_style === "italic";
      const bold = (style.font_weight ?? 400) >= 600;
      lines.push(
        `    SyntaxStyle { capture: ${JSON.stringify(capture)}, color: ${syntaxColor(style.color)}, italic: ${italic}, bold: ${bold} },`,
      );
    }
    lines.push("];", "");
  }

  lines.push("pub mod metrics {");
  for (const metric of metrics()) {
    lines.push(`    pub const ${metric.name}: f32 = ${f32(metric.value)}; // ${metric.note}`);
  }
  for (const [name, points, cssVar] of easings()) {
    lines.push(`    pub const ${name}: [f32; 4] = [${points.map(f32).join(", ")}]; // ${cssVar}`);
  }
  lines.push("}", "");
  return lines.join("\n");
}

function renderMarkdown(): string {
  const light = palette({ appearance: "light", theme: null });
  const dark = palette({ appearance: "dark", theme: null });
  const cell = (value: number) => `\`#${value.toString(16).padStart(8, "0").replace(/ff$/, "")}\``;
  const rows = ["| Native token | Web source | LIGHT | DARK |", "| --- | --- | --- | --- |"];
  for (const token of COLOR_TOKENS) {
    const scope = token.scope === "sidebar" ? " (sidebar scope)" : "";
    rows.push(
      `| \`${token.name}\` | \`${token.web}\`${scope} | ${cell(light.get(token.name)!)} | ${cell(dark.get(token.name)!)} |`,
    );
  }
  return `${rows.join("\n")}\n`;
}

const args = new Set(process.argv.slice(2));
if (args.has("--markdown")) {
  process.stdout.write(renderMarkdown());
} else {
  const output = NodePath.join(ROOT, OUTPUT);
  const rendered = renderRust();
  if (args.has("--check")) {
    const current = NodeFS.existsSync(output) ? NodeFS.readFileSync(output, "utf8") : null;
    if (current !== rendered) {
      console.error(`${OUTPUT} is stale. Run: node apps/native/scripts/gen-theme.ts`);
      process.exit(1);
    }
    console.log(`${OUTPUT} is up to date.`);
  } else {
    NodeFS.mkdirSync(NodePath.dirname(output), { recursive: true });
    NodeFS.writeFileSync(output, rendered);
    console.log(`Wrote ${OUTPUT}`);
  }
}

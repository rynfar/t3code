// Cross-language fixtures from the web editor's actual color-family implementation.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeModule from "node:module";
import * as NodeURL from "node:url";
const native = NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "..");
const web = NodePath.resolve(native, "../web");
const require = NodeModule.createRequire(NodePath.join(web, "package.json"));
require("culori/css");
const { parse, converter } = require("culori/fn");
const source = NodeFS.readFileSync(NodePath.join(web, "src/themePalette.ts"), "utf8");
const functions = source.slice(
  source.indexOf("const THEME_LIGHT_FOREGROUND"),
  source.indexOf("const BUILT_IN_THEME_DEFINITIONS"),
);
const data = JSON.parse(
  NodeFS.readFileSync(NodePath.join(native, "crates/core/src/themes/data.json"), "utf8"),
);
const { updateThemeColorFamily, themeColorToHex } = new Function(
  "parse",
  "converter",
  "T3_CHAT_THEME",
  NodeModule.stripTypeScriptTypes(functions).replaceAll("export ", "") +
    ";return {updateThemeColorFamily,themeColorToHex};",
)(parse, converter, data.themes[0]);
const panel = NodeFS.readFileSync(
  NodePath.join(web, "src/components/settings/ThemeEditorPanel.tsx"),
  "utf8",
);
const families = [
  ...panel
    .slice(
      panel.indexOf("const THEME_EDITOR_ROLE_GROUPS"),
      panel.indexOf("const THEME_EDITOR_COLOR_FAMILIES"),
    )
    .matchAll(/role: "([^"]+)",\s*roles: \[([^\]]+)\]/g),
].map((m) => ({ role: m[1], roles: [...m[2]!.matchAll(/"([^"]+)"/g)].map((m) => m[1]!) }));
const fixtures = [false, true].flatMap((dark) =>
  families.flatMap((f) =>
    ["#734fa080", "oklch(0.62 0.3 280)"].map((value) => {
      const colors = updateThemeColorFamily(
        dark ? "dark" : "light",
        data.standard[Number(dark)],
        f.role,
        value,
      );
      return {
        dark,
        role: f.role,
        value,
        expected: Object.fromEntries(f.roles.map((role) => [role, themeColorToHex(colors[role])])),
      };
    }),
  ),
);
const output = NodePath.join(native, "crates/core/src/themes/editor/fixtures.json");
const text = JSON.stringify(fixtures, null, 2) + "\n";
if (process.argv.includes("--check")) {
  if (NodeFS.readFileSync(output, "utf8") !== text)
    throw new Error("Theme editor fixtures drifted");
} else NodeFS.writeFileSync(output, text);

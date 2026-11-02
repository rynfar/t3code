// `packages/client-runtime/src/work-log/itemDetail.ts` on items sent as `orchestration.getTurnItem`
// results, and the runs `@pierre/highlights` gives JSON text (the inspector's highlighted
// arguments), named by capture through a theme that colors each capture apart.
import * as NodeFS from "node:fs";
import * as NodeModule from "node:module";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

import { at, turnItem } from "./builders.ts";
import type { Item } from "./runtime.ts";
import { ItemDetail, ROOT, encodeTurnItemResult } from "./runtime.ts";

type Highlights = typeof import("@pierre/highlights");

const HIGHLIGHTS = NodeModule.createRequire(
  NodePath.join(
    NodeFS.realpathSync(NodePath.join(ROOT, "apps/web/node_modules/@pierre/diffs")),
    "package.json",
  ),
).resolve("@pierre/highlights");
const Highlights = (await import(NodeURL.pathToFileURL(HIGHLIGHTS).href)) as Highlights;

const CAPTURE_THEME = {
  name: "captures",
  appearance: "dark",
  style: {
    syntax: Object.fromEntries(
      Highlights.tokenNames.map((name, index) => [
        name,
        `#${(index + 1).toString(16).padStart(6, "0")}`,
      ]),
    ),
  },
} as const;
const CAPTURE_BY_COLOR = new Map(
  Object.entries(CAPTURE_THEME.style.syntax).map(([name, color]) => [color, name]),
);

/** The web's JSON runs as `[text, capture]`, `null` where nothing is captured. */
function jsonRuns(text: string): Array<[string, string | null]> {
  const { tokens } = Highlights.codeToTokens(text, { lang: "json", theme: CAPTURE_THEME });
  return tokens.flatMap((line, index) => [
    ...(index > 0 ? [["\n", null] as [string, string | null]] : []),
    ...line.map((token): [string, string | null] => [
      token.content,
      CAPTURE_BY_COLOR.get(token.color?.toLowerCase() ?? "") ?? null,
    ]),
  ]);
}

function base(id: string, status: Item["status"] = "completed") {
  const {
    type: _type,
    input: _input,
    output: _output,
    exitCode: _exitCode,
    ...fields
  } = turnItem("command", id, 1, { runId: "run-1", status }) as Extract<
    Item,
    { type: "command_execution" }
  >;
  return { ...fields, updatedAt: at(9) };
}

const tool = (id: string, fields: Record<string, unknown>, status?: Item["status"]) =>
  ({
    ...base(id, status),
    type: "dynamic_tool",
    toolName: "linear.search",
    input: {},
    ...fields,
  }) as Item;
const command = (id: string, fields: Record<string, unknown>, status?: Item["status"]) =>
  ({ ...base(id, status), type: "command_execution", input: "cargo test", ...fields }) as Item;
const other = (id: string, fields: Record<string, unknown>) => ({ ...base(id), ...fields }) as Item;

const IMAGE = { type: "image", mimeType: "image/png" };
const QUESTION = {
  id: "q1",
  header: "Database",
  question: "Which database?",
  options: [{ label: "Postgres", description: "Server" }],
};

function items(): Item[] {
  return [
    command("withheld", { outputOmitted: true }),
    command("withheld-live", { outputOmitted: true }, "running"),
    command("envelope", { output: '{"stdout":"out\\n","stderr":"warn","interrupted":false}' }),
    command("envelope-blank", { output: '{"stdout":" ","stderr":"","interrupted":true}' }),
    command("not-envelope", { output: '{"stdout":"only"}' }),
    command("blank-failed", { input: "  ", exitCode: 2 }),
    command("plain", { input: " ls -la\n", output: "a\nb\n", exitCode: 0 }),
    tool("args", {
      input: {
        query: "bug",
        limit: 20,
        clear: "",
        reset: null,
        opts: { deep: true, n: 1.5 },
        "2": "two",
      },
    }),
    tool("summarized", { input: { truncated: true, summary: '{"query":"…"}' } }),
    tool("string-input", { input: '{"a":[1,2,{"b":null}]}' }),
    tool("number-input", { input: 42 }),
    tool("text-json", {
      output: { content: [{ type: "text", text: '{"rows":[{"id":1}],"ok":true}' }] },
    }),
    tool("json-lines", { output: '{"a":1}\n\n{"b":[2]}\n' }),
    tool("not-json", { output: "{almost: json}" }),
    tool("images", {
      output: {
        content: [
          ...Array.from({ length: 8 }, () => IMAGE),
          { type: "text", text: "More:" },
          { ...IMAGE, sizeBytes: 100 },
          { type: "image", source: { type: "base64", media_type: "image/jpeg" } },
          { type: "image", mimeType: "image/svg+xml" },
        ],
      },
    }),
    tool("image-array", {
      output: [{ type: "text", text: "Shot:" }, IMAGE, { type: "image", source: { type: "url" } }],
    }),
    tool("resources", {
      output: [
        { type: "resource_link", uri: "file:///a.txt" },
        { type: "resource", resource: { uri: "file:///b.txt" } },
        { type: "resource", resource: { text: "inline", uri: "file:///c.txt" } },
      ],
    }),
    tool("structured", {
      output: {
        content: [{ type: "text", text: "done" }],
        structuredContent: { x: 1 },
        isError: false,
      },
    }),
    tool("structured-blank", {
      output: { content: [{ type: "text", text: "  " }], structuredContent: { x: 1 } },
    }),
    tool("viewed", { toolName: "view_image", viewedImagePath: "/repo/a.png", output: IMAGE }),
    tool("withheld-tool", { output: "kept", outputOmitted: true }),
    tool("empty-output", { output: {} }),
    tool("numeric-output", { output: 1e21 }),
    tool("nested", { output: { a: { b: { c: { d: { e: { f: "deep" } } } } } } }),
    other("files", {
      type: "file_search",
      pattern: "fn main",
      results: [
        { fileName: "/repo/src/main.rs", line: 3, column: 1, preview: "  fn main() {" },
        { fileName: "/repo/b.rs" },
      ],
    }),
    other("files-pattern", { type: "file_search", pattern: "  " }),
    other("web", {
      type: "web_search",
      patterns: ["gpui list"],
      results: [
        { title: " GPUI ", url: "https://gpui.rs", snippet: " fast " },
        { title: "", url: "https://zed.dev" },
        { snippet: "orphan" },
      ],
    }),
    other("change", {
      type: "file_change",
      fileName: "src/a.rs",
      additions: 1200,
      deletions: 3,
      changes: [
        {
          operation: "rename",
          path: "/repo/b.png",
          oldPath: "a.png",
          fileType: "binary",
          mimeType: "image/png",
        },
      ],
    }),
    other("approval", {
      type: "approval_request",
      requestId: "req-1",
      requestKind: "command",
      prompt: "rm -rf build",
    }),
    other("approval-blank", {
      type: "approval_request",
      requestId: "req-2",
      requestKind: "file-read",
      prompt: " ",
    }),
    other("question", { type: "user_input_request", requestId: "req-3", questions: [QUESTION] }),
    other("notice", { type: "system_notice", message: "Context compacted" }),
    other("error", {
      type: "error",
      failure: { class: "provider_error", message: "Overloaded", code: null, retryable: true },
    }),
    other("todo", {
      type: "todo_list",
      planId: "plan-1",
      steps: [
        { id: "s1", text: "Read", status: "completed" },
        { id: "s2", text: "Write", status: "running" },
      ],
    }),
    other("reasoning", { type: "reasoning", text: "  ", streaming: false }),
    other("plan", {
      type: "proposed_plan",
      planId: "plan-2",
      markdown: "# Plan",
      streaming: false,
    }),
  ];
}

const JSON_TEXTS = [
  '{\n  "a": "x\\"y\\\\z\\n\\u00e9\\/",\n  "n": -1.5e3,\n  "m": 0,\n  "e": 2E+10,\n  "t": true,\n  "f": false,\n  "z": null\n}',
  '[1, {"k": []}, "s", {"nested": {"x": {"y": "zé中文"}}}]',
  '{"a\\"b": {"": ""}, "list": [[], {}], "u": "\\uD83D\\uDE00"}',
  '"just a string"',
  "42",
  '  \n {"spaced" :\t[ true ,false ] }  \n',
  "{a: 1}",
  "[1,]",
  "",
  '{"a":1}\n{"b":2}',
];

export function itemDetailCases() {
  const detail = items().map((item) => {
    const args =
      item.type === "command_execution"
        ? ItemDetail.toolCallLines({ command: item.input })
        : item.type === "dynamic_tool"
          ? ItemDetail.toolCallLines({ args: item.input })
          : null;
    return {
      name: item.id,
      result: encodeTurnItemResult({ item }),
      needsFetch: ItemDetail.turnItemNeedsDetailFetch(item),
      revision: ItemDetail.turnItemDetailRevision(item),
      outputText: ItemDetail.turnItemOutputText(item),
      outputImages: ItemDetail.turnItemOutputImages(item),
      hasDetail: ItemDetail.turnItemHasDetail(item),
      callLines: args,
    };
  });
  const missing = { name: "missing", result: encodeTurnItemResult({ item: null }) };
  const argTexts = detail.flatMap((entry) => entry.callLines?.argsText ?? []);
  const highlights = [...JSON_TEXTS, ...argTexts].map((text) => {
    let isJson = text !== "";
    try {
      JSON.parse(text);
    } catch {
      isJson = false;
    }
    return {
      name: `json ${JSON.stringify(text).slice(0, 40)}`,
      json: text,
      isJson,
      runs: isJson ? jsonRuns(text) : [],
    };
  });
  return [...detail, missing, ...highlights];
}

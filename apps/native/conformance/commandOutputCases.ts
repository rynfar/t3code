// `appendTerminalOutput` on seeded chunks under small budgets (redraws, cursor moves, split
// escapes, every truncation path), `applyCommandOutputFrame` on frames built as the server's
// CommandOutputHub builds them, and `terminalOutputSpans` on the results in both palettes.
import type { OrchestrationV2CommandOutputFrame } from "../../../packages/contracts/src/index.ts";

import { random } from "./builders.ts";
import { CommandOutput, TerminalOutput } from "./runtime.ts";

type Rng = ReturnType<typeof random>;

const ESC = "\u001b";
const WORDS = ["ab", "hello", "é", "中文", "  ", "\t", "ok:", "[x]"];
const SGR = [
  "",
  "0",
  "1",
  "2",
  "3",
  "4",
  "7",
  "9",
  "22",
  "23",
  "24",
  "27",
  "29",
  "31",
  "92",
  "42",
  "103",
  "39",
  "49",
  "1;31",
  "0;33;44",
  "38;5;196",
  "38;5;8",
  "48;5;240",
  "38;5;300",
  "38;5",
  "38;2;10;200;30",
  "48;2;250;250;250",
  "38;2;1",
  "38:2::90:80:70",
  "48:5:21",
  "7;42;30",
];
const CURSOR = ["A", "B", "C", "D", "E", "F", "G", "H", "f", "K", "J", "s", "u"];
const OTHER = [
  `${ESC}7`,
  `${ESC}8`,
  `${ESC}M`,
  `${ESC}(B`,
  `${ESC}=`,
  `${ESC}c`,
  `${ESC}[?25l`,
  `${ESC}[>1u`,
  `${ESC}[=5h`,
  `${ESC}]0;title\u0007`,
  `${ESC}]8;;https://example.com${ESC}\\`,
  `${ESC}]junk\n`,
  `${ESC}Pdcs${ESC}\\`,
  "\u0007",
  "\u0000",
  "\u007f",
  "\u0085",
];

/** `huge` adds a column far past any budget, which only small budgets keep cheap to record. */
function cursorParams(rng: Rng, final: string, huge: boolean): string {
  if (final === "K") return rng.pick(["", "0", "1", "2"]);
  if (final === "J") return rng.pick(["", "0", "1", "2", "3"]);
  if (final === "H" || final === "f")
    return rng.pick(["", "1;1", "2;5", `${rng.int(70)};${rng.int(40)}`]);
  return rng.pick(["", "0", "1", "2", String(rng.int(70)), huge ? "999999999" : "3"]);
}

function token(rng: Rng, astral: boolean, huge: boolean): string {
  const roll = rng.next();
  if (astral) {
    if (roll < 0.5) return rng.pick(["😀", "a😀", "𝒳y", "zz", "é"]).repeat(1 + rng.int(6));
    if (roll < 0.75) return "\n";
    return `${ESC}[${rng.pick(SGR)}m`;
  }
  if (roll < 0.3) return rng.pick(WORDS);
  if (roll < 0.4) return "x".repeat(1 + rng.int(30));
  if (roll < 0.55) return rng.pick(["\n", "\n", "\r", "\b", "\r\n"]);
  if (roll < 0.7) return `${ESC}[${rng.pick(SGR)}m`;
  if (roll < 0.85) {
    const final = rng.pick(CURSOR);
    return `${ESC}[${cursorParams(rng, final, huge)}${final}`;
  }
  if (roll < 0.95) return rng.pick(OTHER);
  // A string payload longer than the pending limit, so only its introducer carries over.
  return `${ESC}]${"p".repeat(70)}${rng.pick(["\u0007", `${ESC}\\`, ""])}`;
}

/** Raw chunks whose boundaries fall anywhere in a sequence, escapes included. */
function chunks(rng: Rng, count: number, astral: boolean, huge: boolean): string[] {
  const out: string[] = [];
  let carry = "";
  for (let index = 0; index < count; index += 1) {
    let chunk = carry;
    const tokens = 1 + rng.int(12);
    for (let at = 0; at < tokens; at += 1) chunk += token(rng, astral, huge);
    // Astral cases split only between tokens so no chunk holds half of a surrogate pair.
    const cut = !astral && rng.chance(0.3) ? rng.int(chunk.length) : chunk.length;
    out.push(chunk.slice(0, cut));
    carry = chunk.slice(cut);
  }
  if (carry) out.push(carry);
  return out;
}

function spans(text: string, dark: boolean) {
  return TerminalOutput.terminalOutputSpans(
    text,
    TerminalOutput.TERMINAL_PALETTES[dark ? "dark" : "light"],
  );
}

function foldCase(seed: number, astral: boolean) {
  const rng = random(seed);
  const maxChars = rng.pick([16, 40, 96, 300, 2000]);
  const dark = rng.chance(0.5);
  let state = TerminalOutput.EMPTY_TERMINAL_OUTPUT;
  const steps = chunks(rng, 30, astral, maxChars <= 300).map((chunk, index, all) => {
    state = TerminalOutput.appendTerminalOutput(state, chunk, maxChars);
    const last = index === all.length - 1;
    return {
      chunk,
      text: state.text,
      truncated: state.truncated,
      pending: state.pending,
      ...(index % 6 === 0 || last ? { spans: spans(state.text, dark) } : {}),
    };
  });
  return { name: `${astral ? "astral" : "fold"} ${seed}`, maxChars, dark, steps };
}

/** Frames as `CommandOutputHub.subscribe` sends them: a resumable tail, coalesced appends, resyncs, then the final output. */
function framesCase(seed: number) {
  const rng = random(seed);
  const raw = chunks(rng, 16, false, false);
  let tail = TerminalOutput.EMPTY_TERMINAL_OUTPUT;
  let all = "";
  let pendingAppend = "";
  const frames: OrchestrationV2CommandOutputFrame[] = [];
  const resync = (): OrchestrationV2CommandOutputFrame => ({
    kind: "replace",
    text: TerminalOutput.terminalOutputResumeText(tail),
    truncated: tail.truncated,
    running: true,
  });
  frames.push(resync());
  for (const chunk of raw) {
    tail = TerminalOutput.appendTerminalOutput(tail, chunk);
    all += chunk;
    pendingAppend += chunk;
    if (rng.chance(0.4)) continue;
    if (rng.chance(0.15)) {
      frames.push(resync());
    } else {
      frames.push({
        kind: "append",
        text: pendingAppend,
        truncated: tail.truncated,
        running: true,
      });
    }
    pendingAppend = "";
  }
  const final = TerminalOutput.normalizeTerminalOutput(all);
  frames.push({ kind: "replace", ...final, running: false });
  const dark = rng.chance(0.5);
  let view = CommandOutput.EMPTY_COMMAND_OUTPUT;
  const steps = frames.map((frame) => {
    view = CommandOutput.applyCommandOutputFrame(view, frame);
    return {
      frame,
      text: view.output.text,
      truncated: view.output.truncated,
      running: view.running,
      pending: view.output.pending,
    };
  });
  return { name: `frames ${seed}`, dark, steps, spans: spans(view.output.text, dark) };
}

export function commandOutputCases() {
  return [
    ...Array.from({ length: 32 }, (_, index) => foldCase(12_000 + index, false)),
    ...Array.from({ length: 8 }, (_, index) => foldCase(12_100 + index, true)),
    ...Array.from({ length: 12 }, (_, index) => framesCase(12_200 + index)),
  ];
}

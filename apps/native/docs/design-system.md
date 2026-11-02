# Native design system

The web client is the source of truth. This page maps it to GPUI so native screens match it exactly.
Everything is measured at the default interface font size of 16px.

- Generated tokens: [`crates/app/src/theme/tokens.rs`](../crates/app/src/theme/tokens.rs) contains
  `Palette`, `LIGHT`, `DARK`, `BUILT_IN_THEMES` and `metrics`. Never edit it by hand.
- Regenerate: `node apps/native/scripts/gen-theme.ts`. Verify (CI and parity loop): `--check`. Print
  the color table below: `--markdown`.
- Generator inputs: `apps/web/src/index.css`, `packages/shared/src/themePalettes.ts`,
  `apps/web/src/themePalette.ts` (role → CSS variable names), `packages/contracts/src/settings.ts`
  (font-size settings), Tailwind's `theme.css` (the palette and scales), and the `@pierre/highlights`
  syntax palettes the web's code surfaces use (`SYNTAX_DARK` = pierre-dark, `SYNTAX_LIGHT` = t3-light from
  `lib/readableSyntaxTheme.ts`), applied as gpui-component's highlight theme. It evaluates the real
  cascade (`var()`, `color-mix()`, `light-dark()`, `--alpha()`, oklch) and per-channel clips to sRGB,
  so the hex values match Tailwind's published values and what Chromium paints on an sRGB display.
- If a section here and `tokens.rs` disagree, `tokens.rs` wins.

Read [§10 GPUI rules](#10-gpui-implementation-rules) before writing a view.

## 1. Units

Source: `index.css`, `appearanceFonts.ts`, `packages/contracts/src/settings.ts`.

| Rule                       | Value                                                                                                                                                     |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Spacing unit (`--spacing`) | 4px. Tailwind `p-3` = 12px, `gap-1.5` = 6px                                                                                                               |
| rem                        | = interface font size: default 16px, range 12–20 (Settings → Appearance). Every `rem` metric scales with it                                               |
| px metrics                 | Do not scale: borders, `--workspace-topbar-height` 52px, scrollbar 6px, code font size, arbitrary `text-[11px]`                                           |
| `tokens.rs` tags           | Each `metrics` const ends in `// rem: …` (scale it) or `// px: …` (don't)                                                                                 |
| Desktop sizes              | Use the `sm:` (≥640px) value of every primitive. Below that, the web switches to touch sizes (`h-9` instead of `sm:h-8`)                                  |
| `/NN` modifiers            | `bg-primary/90` compiles to `color-mix(in oklab, primary 90%, transparent)`. It keeps the RGB and multiplies alpha by 0.9 (including a token's own alpha) |
| Box model                  | Border-box, like GPUI. Horizontal padding is usually `n*4 - 1px` so that the 1px border plus the padding land on the 4px grid. Keep the `-1`              |

## 2. Color

### 2.1 Token reference

Source: `index.css` (`:root` 91–152, 1113–1229, 1282–1301, 1600–1712, 2273–2303; `[data-app-sidebar]`
1234–1278; utilities 364–467); `packages/shared/src/themePalettes.ts`; component sources named in the
Web source column. `#rrggbb` is opaque and `#rrggbbaa` is translucent. Tokens marked _sidebar scope_
are evaluated inside `[data-app-sidebar]`, which redefines the surface variables (§2.3).

| Native token                     | Web source                                             | LIGHT       | DARK        |
| -------------------------------- | ------------------------------------------------------ | ----------- | ----------- |
| `app_chrome_background`          | `--app-chrome-background`                              | `#fcfcfc`   | `#000000`   |
| `background`                     | `--background`                                         | `#fcfcfc`   | `#000000`   |
| `foreground`                     | `--color-foreground`                                   | `#18181b`   | `#f5f5f5`   |
| `surface_raised`                 | `--surface-raised`                                     | `#ffffff33` | `#111111`   |
| `card`                           | `--card`                                               | `#ffffff`   | `#111111`   |
| `card_foreground`                | `--color-card-foreground`                              | `#18181b`   | `#f5f5f5`   |
| `popover`                        | `--popover`                                            | `#ffffff`   | `#111111`   |
| `popover_foreground`             | `--color-popover-foreground`                           | `#18181b`   | `#f5f5f5`   |
| `primary`                        | `--primary`                                            | `#1b4ed8`   | `#346bf1`   |
| `primary_foreground`             | `--primary-foreground`                                 | `#ffffff`   | `#ffffff`   |
| `secondary`                      | `--secondary`                                          | `#fafafa`   | `#ffffff08` |
| `secondary_foreground`           | `--color-secondary-foreground`                         | `#27272a`   | `#f5f5f5`   |
| `muted`                          | `--muted`                                              | `#fafafa`   | `#ffffff08` |
| `muted_foreground`               | `--color-muted-foreground`                             | `#71717b`   | `#a1a1a1`   |
| `accent`                         | `--accent`                                             | `#f4f4f5`   | `#ffffff0a` |
| `accent_foreground`              | `--color-accent-foreground`                            | `#18181b`   | `#f5f5f5`   |
| `placeholder`                    | `--color-placeholder`                                  | `#71717b`   | `#a1a1a1`   |
| `secondary_label`                | `--color-secondary-label`                              | `#71717b`   | `#a1a1a1`   |
| `icon_muted`                     | `--color-icon-muted`                                   | `#71717b`   | `#a1a1a1`   |
| `border`                         | `--color-border`                                       | `#e4e4e7`   | `#ffffff14` |
| `input`                          | `--color-input`                                        | `#d4d4d8`   | `#ffffff1c` |
| `ring`                           | `--ring`                                               | `#1b4ed8`   | `#346bf1`   |
| `destructive`                    | `--destructive`                                        | `#fb2c36`   | `#fb414a`   |
| `destructive_foreground`         | `--destructive-foreground`                             | `#c10007`   | `#ff6467`   |
| `error`                          | `--error`                                              | `#fb2c36`   | `#fb414a`   |
| `error_foreground`               | `--error-foreground`                                   | `#c10007`   | `#ff6467`   |
| `error_surface`                  | `--error-surface`                                      | `#fb2c3614` | `#fb414a29` |
| `tool_error_icon`                | `--tool-error-icon`                                    | `#fb2c36`   | `#fca5a5`   |
| `warning`                        | `--warning`                                            | `#fe9a00`   | `#fe9a00`   |
| `warning_foreground`             | `--warning-foreground`                                 | `#bb4d00`   | `#ffb900`   |
| `warning_surface`                | `--warning-surface`                                    | `#fe9a0014` | `#fe9a0029` |
| `success`                        | `--success`                                            | `#00bc7d`   | `#00bc7d`   |
| `success_foreground`             | `--success-foreground`                                 | `#007a55`   | `#00d492`   |
| `info`                           | `--info`                                               | `#2b7fff`   | `#2b7fff`   |
| `info_foreground`                | `--info-foreground`                                    | `#1447e6`   | `#51a2ff`   |
| `update`                         | `--update`                                             | `#1b4ed8`   | `#346bf1`   |
| `update_foreground`              | `--update-foreground`                                  | `#1b4ed8`   | `#51a2ff`   |
| `guest`                          | `--guest`                                              | `#e12afb`   | `#e12afb`   |
| `guest_foreground`               | `--guest-foreground`                                   | `#c800de`   | `#f4a8ff`   |
| `guest_surface`                  | `--guest-surface`                                      | `#ead0ef`   | `#391f3d`   |
| `message_surface`                | `--message-surface`                                    | `#ebebed`   | `#1e1e1e`   |
| `message_foreground`             | `--color-message-foreground`                           | `#18181b`   | `#f5f5f5`   |
| `message_action`                 | `--message-action`                                     | `#1b4ed8`   | `#346bf1`   |
| `message_action_foreground`      | `--message-action-foreground`                          | `#ffffff`   | `#ffffff`   |
| `message_action_hover`           | `--message-action-hover`                               | `#3160db`   | `#2f60d9`   |
| `code_background`                | `--code-background`                                    | `#ffffff`   | `#101010`   |
| `code_foreground`                | `--code-foreground`                                    | `#18181b`   | `#f5f5f5`   |
| `code_block_background`          | `.chat-markdown-codeblock`                             | `#fafafa`   | `#ffffff09` |
| `code_block_border`              | `.chat-markdown-codeblock`                             | `#e4e4e7b3` | `#00000000` |
| `inline_code_background`         | `.chat-markdown :not(pre) > code`                      | `#18181b1a` | `#f5f5f51a` |
| `diff_addition`                  | `--diff-addition`                                      | `#00bc7d`   | `#00bc7d`   |
| `diff_deletion`                  | `--diff-deletion`                                      | `#fb2c36`   | `#fb414a`   |
| `diff_addition_foreground`       | `--diff-addition-foreground`                           | `#007a55`   | `#00d492`   |
| `diff_deletion_foreground`       | `--diff-deletion-foreground`                           | `#c10007`   | `#ff6467`   |
| `diff_modified_icon`             | `--diff-modified-icon`                                 | `#425cb1`   | `#51a2ffa6` |
| `diff_row_addition`              | `diffRendering.ts --diffs-bg-addition-override`        | `#ecf9f4`   | `#02110b`   |
| `diff_row_addition_gutter`       | `diffRendering.ts --diffs-bg-addition-number-override` | `#eef9f5`   | `#04110b`   |
| `diff_row_deletion`              | `diffRendering.ts --diffs-bg-deletion-override`        | `#fef0f0`   | `#170705`   |
| `diff_row_deletion_gutter`       | `diffRendering.ts --diffs-bg-deletion-number-override` | `#fef1f0`   | `#170806`   |
| `diff_hunk_separator`            | `StyledDiffCodeView.tsx --diffs-bg-separator`          | `#f5f5f6`   | `#070707`   |
| `toolbar_background`             | `--toolbar-background`                                 | `#fcfcfc`   | `#000000`   |
| `toolbar_foreground`             | `--contrast-toolbar-foreground`                        | `#18181b`   | `#f5f5f5`   |
| `toolbar_border`                 | `--contrast-toolbar-border`                            | `#e4e4e7`   | `#ffffff14` |
| `toolbar_control`                | `--toolbar-control`                                    | `#ffffff`   | `#111111`   |
| `toolbar_control_foreground`     | `--contrast-toolbar-control-foreground`                | `#18181b`   | `#f5f5f5`   |
| `toolbar_control_hover`          | `--toolbar-control-hover`                              | `#f4f4f5`   | `#ffffff0a` |
| `terminal_background`            | `--terminal-background`                                | `#fcfcfc`   | `#000000`   |
| `terminal_foreground`            | `--terminal-foreground`                                | `#18181b`   | `#f5f5f5`   |
| `terminal_cursor`                | `--terminal-cursor`                                    | `#26384e`   | `#b4cbff`   |
| `terminal_selection`             | `--terminal-selection-background`                      | `#253f6333` | `#b4cbff40` |
| `scrollbar_thumb`                | `--app-scrollbar-thumb`                                | `#d9d9d9`   | `#ffffff14` |
| `scrollbar_thumb_hover`          | `--app-scrollbar-thumb-hover`                          | `#bfbfbf`   | `#ffffff1f` |
| `search_match_background`        | `::highlight(t3-thread-find)`                          | `#ffde8b`   | `#8c6c1f`   |
| `search_match_foreground`        | `::highlight(t3-thread-find)`                          | `#21180a`   | `#f5f5f5`   |
| `search_match_active_background` | `::highlight(t3-thread-find-active)`                   | `#eea52b`   | `#eea52b`   |
| `search_match_active_foreground` | `::highlight(t3-thread-find-active)`                   | `#21180a`   | `#21180a`   |
| `status_input_icon`              | `text-indigo-600 dark:text-indigo-300`                 | `#4f39f6`   | `#a3b3ff`   |
| `terminal_running`               | `text-teal-600 dark:text-teal-300/90`                  | `#009689`   | `#46ecd5e6` |
| `pill_approval`                  | `text-amber-600 dark:text-amber-300/90`                | `#e17100`   | `#ffd230e6` |
| `pill_approval_dot`              | `bg-amber-500 dark:bg-amber-300/90`                    | `#fe9a00`   | `#ffd230e6` |
| `pill_input`                     | `text-indigo-600 dark:text-indigo-300/90`              | `#4f39f6`   | `#a3b3ffe6` |
| `pill_input_dot`                 | `bg-indigo-500 dark:bg-indigo-300/90`                  | `#615fff`   | `#a3b3ffe6` |
| `pill_working`                   | `text-sky-600 dark:text-sky-300/80`                    | `#0084d1`   | `#74d4ffcc` |
| `pill_working_dot`               | `bg-sky-500 dark:bg-sky-300/80`                        | `#00a6f4`   | `#74d4ffcc` |
| `pill_failed`                    | `text-red-600 dark:text-red-300/90`                    | `#e7000b`   | `#ffa2a2e6` |
| `pill_failed_dot`                | `bg-red-500 dark:bg-red-300/90`                        | `#fb2c36`   | `#ffa2a2e6` |
| `pill_plan`                      | `text-violet-600 dark:text-violet-300/90`              | `#7f22fe`   | `#c4b4ffe6` |
| `pill_plan_dot`                  | `bg-violet-500 dark:bg-violet-300/90`                  | `#8e51ff`   | `#c4b4ffe6` |
| `pill_completed`                 | `text-emerald-600 dark:text-emerald-300/90`            | `#009966`   | `#5ee9b5e6` |
| `pill_completed_dot`             | `bg-emerald-500 dark:bg-emerald-300/90`                | `#00bc7d`   | `#5ee9b5e6` |
| `usage_codex`                    | `usageProviders.ts codex`                              | `#18181b`   | `#f5f5f5`   |
| `usage_claude`                   | `usageProviders.ts claude`                             | `#d97757`   | `#d97757`   |
| `usage_grok`                     | `usageProviders.ts grok`                               | `#4f4f52`   | `#9e9e9e`   |
| `usage_cursor`                   | `usageProviders.ts cursor`                             | `#8b8b8b`   | `#8b8b8b`   |
| `usage_opencode`                 | `usageProviders.ts opencode`                           | `#5b9bbd`   | `#5b9bbd`   |
| `usage_antigravity`              | `usageProviders.ts antigravity`                        | `#8c7bd1`   | `#8c7bd1`   |
| `usage_pi`                       | `usageProviders.ts pi`                                 | `#4d9f8a`   | `#4d9f8a`   |
| `usage_muse`                     | `usageProviders.ts muse`                               | `#0866ff`   | `#0866ff`   |
| `usage_ink_30`                   | `usageBreakdown.ts ink(30)`                            | `#b0b0b1`   | `#2b2b2b`   |
| `usage_ink_34`                   | `usageBreakdown.ts ink(34)`                            | `#a6a6a8`   | `#353535`   |
| `usage_ink_44`                   | `usageBreakdown.ts ink(44)`                            | `#8e8e90`   | `#4f4f4f`   |
| `usage_ink_60`                   | `usageBreakdown.ts ink(60)`                            | `#69696c`   | `#7b7b7b`   |
| `usage_ink_66`                   | `usageBreakdown.ts ink(66)`                            | `#5c5c5f`   | `#8c8c8c`   |
| `usage_ink_72`                   | `usageBreakdown.ts ink(72)`                            | `#4f4f52`   | `#9e9e9e`   |
| `usage_ink_100`                  | `usageBreakdown.ts ink(100)`                           | `#18181b`   | `#f5f5f5`   |
| `glass_surface`                  | `@utility surface-glass`                               | `#fcfcfccc` | `#000000cc` |
| `dialog_surface`                 | `@utility dialog-glass`                                | `#fcfcfccc` | `#000000cc` |
| `dialog_border`                  | `@utility dialog-glass`                                | `#18181b1a` | `#ffffff14` |
| `dialog_backdrop`                | `@utility dialog-backdrop`                             | `#fcfcfc99` | `#000000a3` |
| `dropdown_surface`               | `@utility dropdown-glass`                              | `#ffffffd5` | `#111111d5` |
| `dropdown_border`                | `@utility dropdown-glass`                              | `#18181b1a` | `#f5f5f51a` |
| `sidebar`                        | `--sidebar` (sidebar scope)                            | `#fafafa`   | `#000000`   |
| `sidebar_foreground`             | `--color-sidebar-foreground` (sidebar scope)           | `#18181b`   | `#f1f3f7`   |
| `sidebar_muted_foreground`       | `--color-sidebar-muted-foreground` (sidebar scope)     | `#71717b`   | `#a3a3a3`   |
| `sidebar_icon`                   | `--sidebar-icon-color` (sidebar scope)                 | `#86858e`   | `#8b8b8b`   |
| `sidebar_secondary_label`        | `--color-secondary-label` (sidebar scope)              | `#71717b`   | `#8f8f8f`   |
| `sidebar_accent`                 | `--accent` (sidebar scope)                             | `#f4f4f5`   | `#191a1d`   |
| `sidebar_control_surface`        | `--sidebar-control-surface` (sidebar scope)            | `#f4f4f5`   | `#0a0a0a`   |
| `sidebar_row_hover`              | `--sidebar-row-hover` (sidebar scope)                  | `#fcfcfc`   | `#f1f3f714` |
| `sidebar_row_active`             | `--sidebar-row-active` (sidebar scope)                 | `#ffffff`   | `#f1f3f71c` |
| `sidebar_row_selected`           | `--sidebar-row-selected` (sidebar scope)               | `#ffffff`   | `#f1f3f712` |
| `sidebar_border`                 | `--color-sidebar-border` (sidebar scope)               | `#e4e4e7`   | `#ffffff14` |
| `sidebar_edge`                   | `[data-app-sidebar] border-color` (sidebar scope)      | `#e4e4e7`   | `#ffffff14` |

### 2.2 What each family is for

| Family      | Tokens                                                                               | Use for                                                                                                                                                            |
| ----------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Window      | `app_chrome_background`                                                              | Window fill behind everything, including the titlebar strip. Equals `background` in the default theme; named themes can differ                                     |
| Canvas      | `background`, `foreground`                                                           | Main content area and its text                                                                                                                                     |
| Raised      | `card`, `surface_raised`, `popover` (+ `_foreground`)                                | Cards and settings groups (`card` at 40%). Composer glass is `card` in light and `surface_raised` in dark. `popover` is the opaque fallback for menus and tooltips |
| Quiet fills | `secondary`, `muted`, `accent`                                                       | `accent`: hover and highlighted row fill. `secondary`: secondary buttons and code-block fill. `muted`: dialog footer (`/72`), quiet panels                         |
| Text tiers  | `foreground` > `secondary_label` = `muted_foreground` = `placeholder` = `icon_muted` | Keep the four muted roles separate: named themes give them different values                                                                                        |
| Lines       | `border`, `input`, `ring`                                                            | `border`: dividers and cards. `input`: control outlines, switch-off track. `ring`: focus                                                                           |
| Status      | `error*`, `warning*`, `success*`, `info*`, `update*`, `guest*`, `destructive*`       | `*_foreground` for text and icons; `*_surface` for tinted fills. `destructive` = `error`                                                                           |
| Transcript  | `message_*`, `code_*`, `inline_code_background`, `search_match_*`                    | User bubble and send button (`message_action`), code blocks, find-in-thread highlights                                                                             |
| Diff        | `diff_*`                                                                             | Diff panel and stats. Background mixes are in §7.8                                                                                                                 |
| Toolbar     | `toolbar_*`                                                                          | Chat-header controls under named themes only (`html[data-theme-id] [data-chat-header]`). The default theme uses plain button variants, whose colors are the same   |
| Status hues | `status_input_icon`, `terminal_running`, `pill_*`                                    | Hues the web hard-codes as Tailwind classes. They are not themeable                                                                                                |
| Usage       | `usage_<provider>`, `usage_ink_*`                                                    | Usage page series, dots and limit bars per provider; share-bar segments (foreground mixed into the background)                                                     |
| Glass       | `glass_surface`, `dialog_*`, `dropdown_*`                                            | Fills to use with backdrop blur (§4.6). `dialog_backdrop` is the modal scrim                                                                                       |
| Sidebar     | `sidebar*`                                                                           | Everything inside the app sidebar. Ghost-button hover inside the sidebar is `sidebar_accent`, not `accent`                                                         |

### 2.3 Color rules

- **Never flatten translucent tokens.** In dark mode `secondary`, `muted`, `accent`, `border`, `input`
  and the sidebar row states are white at 3–11% alpha. They must composite over whatever is beneath,
  so a hovered row on a card differs from one on the canvas.
- **The sidebar is its own scope.** `[data-app-sidebar]` redefines `--background`, `--accent`,
  `--muted`, `--border` and more (`index.css:1234–1278`). In dark mode the sidebar is true black with
  `#191a1d` accents. Views inside the sidebar read `sidebar_*` tokens, plus `foreground`, which is
  `#f1f3f7` there.
- **Settings navigation is not the app sidebar.** It uses the root `--sidebar`: `sidebar` in light,
  `card` in dark.
- **Contrast setting.** The range is 50–200, default 100 (`appearanceContrast.ts`, `index.css:1600–1712`):
  `fg' = mix_oklab(mix_oklab(fg, bg, base), target, boost)`, where `base = min(c,100)%`,
  `boost = max(c-100,0)%` and the target is black (light) or white (dark). Borders and inputs use
  `mix_srgb(mix_srgb(border, transparent, base), foreground, boost/4)`. At 100 this is the identity,
  which is what `tokens.rs` stores.
- **Diff color scheme setting**: `red-green` (default, the tokens) or `blue-orange`. Additions become
  blue-600/400 and deletions orange-600/400, with `diff_*_foreground` equal to `diff_*`
  (`index.css:2290–2303`).

### 2.4 How themes work on the web

Sources: `apps/web/src/themePalette.ts`, `apps/web/src/hooks/useTheme.ts`,
`packages/shared/src/themePalettes.ts`, `index.css:1318–1389, 1563–1597`.

| Concept                   | Web behavior                                                                                                                                                                                       |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Default theme ("T3 Code") | No `data-theme-id`. Painted directly by `index.css` `:root`, Tailwind zinc/neutral. Stored preference `t3code:theme` = `system` \| `light` \| `dark`                                               |
| Appearance mode           | `t3code:theme-appearance-mode` = `light` \| `dark` \| `system`. System follows `prefers-color-scheme`                                                                                              |
| Built-in named themes     | `t3-chat`, `grove`, `ocean`, `ember`, `iris` (`BUILT_IN_THEMES`). Each is a 60-role palette (`THEME_COLOR_ROLES`) with light and dark variants, all with `sidebarArtwork`                          |
| Applying a named theme    | Sets `data-theme-id` and `--app-theme-<role>` on `<html>`. `index.css` maps roles to semantic variables (table §2.6). Error, warning and update follow the theme; success, info and guest never do |
| Halves                    | `t3code:theme-halves:v1` can pick a different theme for light and for dark                                                                                                                         |
| Custom themes             | Imported JSON (`version: 1`, `id`, `name`, `appearance`, `colors`, optional `variants`). Missing roles fall back to the **T3 Chat** palette (`getDefaultThemeColors`), not to the T3 Code default  |
| Environment themes        | Published by the server over the wire (`EnvironmentTheme`). Seeded ones (`canvas` + `accent`) are expanded by `createVividThemeColors`                                                             |
| Switching                 | Atomic. A `no-transitions` class suppresses every transition for the frame                                                                                                                         |
| Theme-specific CSS        | A few rules key on the id: the T3 Chat light inline-code fill (folded into `inline_code_background`), the T3 Chat sidebar edge, sidebar artwork and the grain tile                                 |

### 2.5 Native support plan

1. **Default first.** Ship `tokens::LIGHT` / `tokens::DARK`. Resolve `system` from the window
   appearance (`window.appearance()` / `observe_window_appearance`) and swap the palette atomically.
   Don't cross-fade.
2. **Named themes.** `tokens::BUILT_IN_THEMES` is generated from the shared list, so a theme added
   there appears after regenerating. Store preferences with the web's vocabulary (theme id string,
   appearance mode, halves) so a future settings sync is lossless.
3. **Custom and environment themes (later).** These need the role → token mapping (§2.6) at runtime in
   Rust, plus T3 Chat fallback for missing roles. Port `createVividThemeColors` only when seeded
   environment themes are needed.
4. **Contrast, glass opacity and diff scheme (later).** Apply them as transforms over the active
   `Palette` (§2.3, §4.6). The generated values are exact at the defaults (100 / 80% / red-green).

### 2.6 Role → token mapping (named, custom and environment themes)

Source: `index.css:1325–1389` (root) and `1563–1589` (sidebar scope).

| Token                                                           | Role                                                                                         | Token                                             | Role                                                                             |
| --------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | ------------------------------------------------- | -------------------------------------------------------------------------------- |
| `background`                                                    | `canvas`                                                                                     | `app_chrome_background`                           | `chrome`                                                                         |
| `foreground`, `card_foreground`, `popover_foreground`           | `text`                                                                                       | `surface_raised`                                  | `surfaceRaised`                                                                  |
| `card`                                                          | `surface`                                                                                    | `popover`                                         | `surfaceOverlay`                                                                 |
| `primary`, `message_action`                                     | `messageAction`                                                                              | `primary_foreground`, `message_action_foreground` | `messageActionForeground`                                                        |
| `message_action_hover`                                          | `messageActionHover`                                                                         | `message_surface` / `message_foreground`          | `messageSurface` / `messageForeground`                                           |
| `secondary` / `secondary_foreground`                            | `secondary` / `secondaryForeground`                                                          | `muted` / `muted_foreground`                      | `muted` / `mutedForeground`                                                      |
| `accent`, `sidebar_accent`                                      | `accentSurface`                                                                              | `accent_foreground`                               | `accentSurfaceForeground`                                                        |
| `placeholder`                                                   | `placeholder`                                                                                | `secondary_label`, `sidebar_secondary_label`      | `secondaryLabel`                                                                 |
| `icon_muted`                                                    | `iconMuted`                                                                                  | `ring`                                            | `focus`                                                                          |
| `border` / `input`                                              | `border` / `input`                                                                           | `error`, `destructive`, `diff_deletion`           | `error`                                                                          |
| `error_foreground`, `destructive_foreground`, `tool_error_icon` | `errorForeground`                                                                            | `error_surface`                                   | `errorSurface`                                                                   |
| `warning` / `warning_foreground` / `warning_surface`            | `warning*`                                                                                   | `update` / `update_foreground`                    | `update` / `updateForeground`                                                    |
| `code_background` / `code_foreground`                           | `codeBackground` / `codeForeground`                                                          | `code_block_background` / `code_block_border`     | `codeBackground` / `border`                                                      |
| `toolbar_*`                                                     | `toolbar*` (same suffix)                                                                     | `terminal_*`                                      | `terminal*` (`terminal_selection` ← `terminalSelection`)                         |
| `search_match_*`                                                | `searchMatch*`                                                                               | `sidebar`, `sidebar_*`                            | `sidebar*` (same suffix)                                                         |
| `sidebar_edge`                                                  | `color-mix(sidebar_foreground 10%, transparent)`, 8% dark; T3 Chat dark uses `sidebarBorder` | not mapped                                        | `textMuted`, `accent`, `accentForeground`, `updateSurface`, `terminalScrollbar*` |

## 3. Typography

Sources: `index.css:170–190, 1714–1720, 1787–1801, 1893–2137`, `appearanceFonts.ts`,
`packages/contracts/src/settings.ts:155–185`, components as cited.

### 3.1 Families and size settings

| Role                         | Web stack                                                                                  | Native                                                                       |
| ---------------------------- | ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| UI (`--font-sans`)           | `-apple-system, BlinkMacSystemFont, "Inter Variable", "Segoe UI", system-ui, sans-serif`   | `.SystemUIFont` (GPUI's platform UI font: SF Pro on macOS)                   |
| Code (`--font-mono`)         | `ui-monospace, "SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace` | `"SF Mono"`, then `Menlo` (macOS), `Consolas` (Windows), `"Liberation Mono"` |
| Composer (`--font-composer`) | Custom family only when advanced typography is on; otherwise the UI font                   | Same                                                                         |
| Terminal                     | Code family unless advanced typography is on                                               | Same                                                                         |

| Setting        | Default                          | Range  | Applies to                                                                                                       |
| -------------- | -------------------------------- | ------ | ---------------------------------------------------------------------------------------------------------------- |
| Interface size | 16px (`metrics::ROOT_FONT_SIZE`) | 12–20  | rem base. Scales every rem metric                                                                                |
| Prompt size    | 14px (`text-sm`)                 | 12–20  | Composer. Only honored with advanced typography; otherwise `text-sm`, which still scales with the interface size |
| Code size      | 13px                             | 10–18  | Code blocks, tool output, diffs. Fixed px                                                                        |
| Terminal size  | 12px                             | 8–20   | Terminal                                                                                                         |
| Font smoothing | off                              | toggle | `-webkit-font-smoothing: antialiased`                                                                            |

### 3.2 Scale

Sizes and line heights are px at 16px rem (`metrics::TEXT_*`, `metrics::TEXT_*_LINE_HEIGHT`).

| Step        | 5xs | 4xs | 3xs | 2xs | xs  | sm  | base | lg  | xl  | 2xl | 3xl |
| ----------- | --- | --- | --- | --- | --- | --- | ---- | --- | --- | --- | --- |
| Size        | 7   | 8   | 10  | 11  | 12  | 14  | 16   | 18  | 20  | 24  | 30  |
| Line height | 7   | 8   | 14  | 16  | 16  | 20  | 24   | 28  | 28  | 32  | 36  |

| Property          | Values in use                                                                                                                                                                       |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Weights           | 400 body; **500 (`font-medium`)** most labels, buttons, menu items, badges, thread titles; 600 headings, dialog titles, `strong`; 700 only "New" badges                             |
| Leading overrides | `leading-relaxed` 1.625 (prose, composer, work rows); `leading-snug` 1.375 (tooltips, code blocks, model names); `leading-none` (dialog title, small badges); markdown headings 1.3 |
| Tracking          | None on prose. `tracking-tight` −0.025em on the brand and hero headline; `tracking-widest` 0.1em on menu shortcuts                                                                  |
| Numerals          | `tabular-nums` on timestamps, counters, diff stats, ordered-list markers                                                                                                            |

### 3.3 Which element uses which

| Element                                                | Font               | Size / line                                               | Weight                  | Color                                            |
| ------------------------------------------------------ | ------------------ | --------------------------------------------------------- | ----------------------- | ------------------------------------------------ |
| Default control text                                   | sans               | 14 / 20                                                   | 500                     | per variant                                      |
| Assistant prose (`ChatMarkdown.tsx:1773`)              | sans               | 14 / 22.75                                                | 400 (`strong` 600)      | `foreground` at 90% alpha                        |
| User bubble                                            | sans               | 14 / 22.75                                                | 400                     | `message_foreground`                             |
| Markdown h1 / h2 / h3 / h4–h6                          | sans               | 20 / 18 / 16 / 14, line 1.3, margin 20px above, 8px below | 600                     | `foreground` (h6 `muted_foreground`)             |
| Markdown table                                         | sans               | 14                                                        | th 600                  | rows divided by `border` at 60%                  |
| Inline code                                            | mono               | 0.85em, padding 0.1em 0.35em, radius 4                    | —                       | `foreground` on `inline_code_background`         |
| Code block                                             | mono               | code size (13) / 1.375                                    | —                       | `foreground`                                     |
| Code block language label                              | mono               | 11                                                        | —                       | `foreground` at 72%                              |
| Tool output, expanded                                  | mono               | code size (fallback 11) / 1.625                           | —                       | `secondary_label`                                |
| Diff lines / file header / hunk                        | mono / sans / sans | 13, 20px line / 12 / 11                                   | —                       | —                                                |
| Sidebar thread title                                   | sans               | 14 / 20                                                   | 500 (400 when receding) | §7.2                                             |
| Sidebar row meta (project, time)                       | sans               | 12 / 16, time tabular                                     | 400                     | `sidebar_secondary_label`                        |
| Sidebar section header                                 | sans               | 12 / 16                                                   | 500                     | `sidebar_muted_foreground`                       |
| Sidebar status label                                   | sans               | 12 / 16                                                   | 500                     | status token (§7.3)                              |
| Chat header title                                      | sans               | 14 / 20                                                   | 500                     | `foreground`                                     |
| Composer prompt                                        | composer           | prompt size (14) / 1.625                                  | 400                     | `foreground`; placeholder `placeholder`          |
| Work / tool row label                                  | sans               | 14 / 1.625                                                | 400                     | `secondary_label`                                |
| Timestamps                                             | sans               | 12, tabular                                               | 400                     | `muted_foreground`                               |
| Kbd                                                    | sans               | 12                                                        | 500                     | `muted_foreground`                               |
| Menu shortcut                                          | sans               | 12, tracking 0.1em                                        | 500                     | `secondary_label`                                |
| Small labels (menu group, select group, command group) | sans               | 12                                                        | 500                     | `muted_foreground`                               |
| Tooltip                                                | sans               | 12 / 1.375                                                | 400                     | `popover_foreground`                             |
| Dialog title / description                             | sans               | 20 / 20 (leading-none) / 14                               | 600 / 400               | `popover_foreground` / `muted_foreground`        |
| Settings section title / row title / row description   | sans               | 16 / 14 / 12                                              | 600 / 500 / 400         | `foreground` / `foreground` / `muted_foreground` |
| Command-palette status pill                            | sans               | 10                                                        | 500                     | `pill_*`                                         |
| New-thread hero headline                               | sans               | 30 (`sm:text-3xl`), tracking −0.025em                     | 400                     | `foreground`                                     |

## 4. Spacing, radius, borders, elevation, layers, glass

Sources: `index.css`, Tailwind `theme.css`, primitives in `apps/web/src/components/ui`.

### 4.1 Radius

`--radius` is 8px. `index.css:289–294` overrides Tailwind's sm–3xl.

| Token                          | px  | Used by                                                                              |
| ------------------------------ | --- | ------------------------------------------------------------------------------------ |
| `RADIUS_XS`                    | 2   | —                                                                                    |
| `RADIUS_SM`                    | 4   | Menu, select and command items; badges; checkbox; kbd; inline code; skeleton block   |
| `RADIUS_MD`                    | 6   | Compact/xs controls, thread rows, tabs, tooltips, search field                       |
| `RADIUS_LG` = `CONTROL_RADIUS` | 8   | Buttons, inputs, selects, toggles, menus, popovers, toasts, code blocks              |
| `RADIUS_XL`                    | 12  | Alerts, settings groups                                                              |
| `RADIUS_2XL`                   | 16  | Dialogs, command palette, composer, user bubble, thread-details card, settings modal |
| `RADIUS_3XL`                   | 20  | —                                                                                    |
| full                           | —   | Switch, glass pill buttons, send/stop button, status dots                            |

When a layer sits inside a bordered box (`before:` bevels, footers), its radius is the outer radius minus 1px.

### 4.2 Borders and focus

| Rule                                                           | Value                                                                                                                     |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| Width                                                          | 1px everywhere. Default color `border`; control outlines `input`                                                          |
| Separator                                                      | 1px `border` (`separator.tsx`); menu separator adds 8px horizontal and 4px vertical margin                                |
| Focus ring (buttons, toggles, badges, switch, scroll viewport) | 2px `ring` drawn inside the control, after a 1px `background`-colored gap at the edge (`ring-2 ring-inset ring-offset-1`) |
| Focus (inputs, textareas, select trigger)                      | Border becomes `ring`, plus a 3px inset ring of `ring` at 24%                                                             |
| Focus (checkbox, workspace areas)                              | 2px `ring` outline, offset −2px                                                                                           |
| Invalid                                                        | Border `destructive` at 36% (64% when focused); ring `destructive` at 16% (24% dark)                                      |
| Global outline color                                           | `ring` at 50%, offset −2px (`index.css:714–722`)                                                                          |

### 4.3 Elevation

| Name          | Shadow                                                                                                                                   | Used by                                           |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------- |
| Control       | `0 1px 2px rgb(0 0 0 / 5%)` (`shadow-xs/5`)                                                                                              | Inputs, outline buttons, checkbox, select trigger |
| Control bevel | Inner layer. Light `0 1px rgb(0 0 0 / 4%)`; dark `0 -1px rgb(255 255 255 / 6%)`                                                          | Outline buttons, inputs, popovers, tooltips       |
| Solid button  | Inset `0 1px rgb(255 255 255 / 16%)` plus `0 1px 2px` in the button color at 24%. Pressed: inset `0 1px rgb(0 0 0 / 8%)`, no drop shadow | `default`, `destructive` buttons                  |
| Floating      | Light `0 16px 40px -18px rgb(0 0 0 / 55%)`; dark `0 18px 44px -18px rgb(0 0 0 / 80%)`                                                    | Menus, selects, popovers                          |
| Tooltip       | `0 4px 6px -1px rgb(0 0 0 / 5%), 0 2px 4px -2px rgb(0 0 0 / 5%)`                                                                         | Tooltips                                          |
| Toast         | `0 20px 25px -5px rgb(0 0 0 / 25%), 0 8px 10px -6px rgb(0 0 0 / 25%)`                                                                    | Toasts, glass tooltips                            |
| Dialog        | Light `0 24px 64px -24px rgb(0 0 0 / 65%)`; dark inset `0 1px rgb(255 255 255 / 4%)` plus `0 24px 72px -20px rgb(0 0 0 / 90%)`           | Dialogs, command palette                          |
| Composer      | Light `0 1px 2px rgb(0 0 0 / 5%)`; dark none. Plus a 1px outline of `rgb(0 0 0 / 8%)` (dark: white 5%)                                   | Composer                                          |
| Pill          | `0 1px 3px rgb(0 0 0 / 10%), 0 1px 2px -1px rgb(0 0 0 / 10%)` (`shadow-sm`)                                                              | Glass buttons, scroll-to-bottom                   |

zui's `BoxShadow` supports `inset`, so every row above maps 1:1.

### 4.4 Layers (paint order, low → high)

| Layer                     | Web z                | Notes                                    |
| ------------------------- | -------------------- | ---------------------------------------- |
| Content, sticky headers   | auto / 10–20         |                                          |
| Terminal drawer           | 45                   | Above the timeline minimap               |
| Sheets (`--z-sheet`)      | 46                   | Right-panel sheet, details panel popover |
| Preview mini-player       | 47–49                |                                          |
| Dialog backdrop and popup | 50 (media viewer 60) |                                          |
| Toasts                    | 100                  |                                          |
| Menus, selects, popovers  | 130                  |                                          |
| Tooltips                  | 140                  |                                          |

In GPUI, render overlays with `deferred()` using priorities in this order. Never rely on child order
across separate windows.

### 4.5 Opacity

| Use                                  | Value                                                  |
| ------------------------------------ | ------------------------------------------------------ |
| Disabled control                     | 0.64                                                   |
| Icons in menu items, badges, toggles | 0.8                                                    |
| Select chevron                       | 0.7                                                    |
| Receding sidebar favicon             | 0.4 plus grayscale, until hover                        |
| Skeleton fill                        | `muted_foreground` at 15%                              |
| Scroll-edge fade                     | 24px mask on overflowing edges (`--fade-size: 1.5rem`) |

### 4.6 Glass

| Property                       | Light                                                         | Dark | Source                            |
| ------------------------------ | ------------------------------------------------------------- | ---- | --------------------------------- |
| Blur (`GLASS_BLUR*`)           | 12px                                                          | 16px | `--glass-blur`                    |
| Saturation                     | 1.14                                                          | 1.08 | `--glass-saturation`              |
| Fill opacity (`GLASS_OPACITY`) | 80% (setting 40–100)                                          | same | `--glass-opacity`                 |
| Fallback without blur          | Opaque `background` (surface, dialog) or `popover` (dropdown) | same | `@supports not (backdrop-filter)` |

The glass fills are `dialog_surface` (dialogs, command palette), `dropdown_surface` / `dropdown_border`
(menus, selects, popovers, toasts, thread-details card) and `glass_surface` (glass buttons). The
composer uses `card` (light) or `surface_raised` (dark) at the glass opacity. Paint them over
`window.paint_backdrop_blur(bounds, radii, blur)`, which zui provides. Blur breaks the render pass,
so use it only on floating overlays and the composer, never on list rows.

The web also bakes a 3.5% fractal-noise grain into the chrome background (`index.css:1761–1785`;
T3 Chat 2.745% on a 128px tile). It is static, so native may ship it later as a tiled image. It is
not required for v1.

## 5. Layout metrics (desktop window)

Sources: `index.css`, `components/threadSidebarWidth.ts`, `components/ui/sidebar.tsx`,
`components/AppSidebarLayout.tsx`, `hooks/usePreviewPanelInlineSize.ts`, `rightPanelLayout.ts`,
`components/ThreadTerminalDrawer.tsx`, `components/chat/threadDetailsCardLayout.ts`,
`components/ui/command.tsx`, `components/settings/*`.

| Area                    | Metric                                                                                                                                                                                                                                                                                     |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Titlebar / topbar       | 52px (`WORKSPACE_TOPBAR_HEIGHT`), a drag region. macOS traffic lights reserve 90px on the left. The sidebar toggle sits at x = 91, 28px square; titlebar content starts at 130px (90 + 28 + 12) when the sidebar is collapsed                                                              |
| Titlebar controls       | 28px (`WORKSPACE_TITLEBAR_CONTROL_SIZE`), gap 12px; right cluster inset 12px, gap 4px                                                                                                                                                                                                      |
| Sidebar                 | Default 256, min 208, max = window − 640. Fully hidden when collapsed (offcanvas, no icon rail). `border-r` = `sidebar_edge`. Resize rail: 16px hit area, 2px `sidebar_border` line on hover, double-click resets. Toggle `mod+b`. Under 768px it becomes an overlay sheet (window − 12px) |
| Main column             | Min 640px (`THREAD_MAIN_CONTENT_MIN_WIDTH`)                                                                                                                                                                                                                                                |
| Chat header             | 52px; padding-x 20px (12 under 640); gap 12px                                                                                                                                                                                                                                              |
| Transcript lane         | Max width `CHAT_CONTENT_MAX_WIDTH` 736 (comfortable, default), `…_WIDE` 1152, or full. Centered. Scroller padding-x 48px (12 under 640); top spacer 16px                                                                                                                                   |
| Composer                | Same lane and padding as the transcript. Specs in §7.9                                                                                                                                                                                                                                     |
| Right panel (inline)    | Default 540, min 360, max min(70% of window, row − 360). Width persisted per thread. Edge `border-l`. Resize handle: 8px hit area, 1px line (`border` on hover, `primary` at 60% while dragging). Under 980px it becomes a sheet `min(42vw, 448)`, min 320 (`min(88vw, 384)` under 760px)  |
| Right panel tab bar     | 52px; tabs 24px tall, max 144px wide, radius 6, padding 6 / 8, 12px text, active `accent`, inactive hover `accent` at 60%                                                                                                                                                                  |
| Thread details          | Floating card 280px (`THREAD_DETAILS_PANEL_WIDTH`), inset 12px from top-right, radius 16, dropdown glass. Inline only when the canvas is at least about 1012px; otherwise a popover                                                                                                        |
| Terminal drawer         | Default 280px tall, min 180, max 75% of window. 6px top resize strip; `border-t` = `border` at 80%; session list 144px wide                                                                                                                                                                |
| Dialog                  | Max width 512, viewport padding 16, vertically centered (rows `1fr auto 1fr`)                                                                                                                                                                                                              |
| Command palette         | 576 × max 420, radius 16, top offset 10vh                                                                                                                                                                                                                                                  |
| Settings                | Modal max 1280×960, radius 16, viewport padding 24 (40 at ≥1024). Nav 240px (56px icons-only under 768). Content max 896, padding 24 / 24 / 48                                                                                                                                             |
| Toasts                  | Top-right. Max width 360, inset 32px (16 under 640), top = 32 + 52                                                                                                                                                                                                                         |
| Breakpoints that matter | 640 (`sm`: desktop sizes, gutters), 768 (sidebar → sheet), 980 (right panel → sheet), about 1012 (details card inline)                                                                                                                                                                     |

## 6. Primitives

Source: `apps/web/src/components/ui/*`. All values are desktop (`sm:`) sizes in px.

### 6.1 Button (`button.tsx`)

Base: 1px border, radius 8, 500 weight, 14px text, gap 8, 16px icons, `nowrap`. Press scales to 0.97
except for menu triggers. Disabled opacity 0.64. Focus: ring (§4.2).

| Size                                                   | Height                        | Pad-x  | Text | Icon                   | Notes                 |
| ------------------------------------------------------ | ----------------------------- | ------ | ---- | ---------------------- | --------------------- |
| `default`                                              | 32                            | 11     | 14   | 16                     |                       |
| `xs`                                                   | 24                            | 7      | 12   | 14                     | gap 4                 |
| `sm`                                                   | 28                            | 9      | 14   | 16                     | gap 6                 |
| `lg`                                                   | 36                            | 13     | 14   | 16                     |                       |
| `xl`                                                   | 40                            | 15     | 16   | 18                     |                       |
| `compact`                                              | 28                            | 7      | 12   | 14                     | radius 6, gap 4       |
| `micro`                                                | 20                            | 5      | 11   | 12                     | radius 4, gap 4       |
| `multiline` / `sm-multiline`                           | min 32 / 28                   | 11 / 9 | 14   | 16                     | pad-y 5 / 3, wraps    |
| `icon` / `icon-sm` / `icon-xs` / `icon-lg` / `icon-xl` | 32 / 28 / 24 / 36 / 40 square | —      | —    | 16 / 16 / 14 / 16 / 18 |                       |
| `icon-micro` / `icon-tiny`                             | 20 / 16 square                | —      | —    | 12                     | `icon-micro` radius 4 |

| Variant               | Rest                                                                            | Hover / pressed                                           | Text                     | Icon               |
| --------------------- | ------------------------------------------------------------------------------- | --------------------------------------------------------- | ------------------------ | ------------------ |
| `default`             | `primary` fill and border, solid-button shadow                                  | `primary` at 90%                                          | `primary_foreground`     | current            |
| `destructive`         | `destructive` fill and border, solid-button shadow                              | `destructive` at 90%                                      | white                    | current            |
| `outline`             | `input` border; fill `popover` (dark: `input` at 32%); control shadow and bevel | `accent` at 50% (dark: `input` at 64%)                    | `foreground`             | `muted_foreground` |
| `destructive-outline` | as `outline`                                                                    | border `destructive` at 32%, fill `destructive` at 4%     | `destructive_foreground` | current            |
| `secondary`           | `secondary`, no visible border                                                  | `secondary` at 90% (pressed 80%)                          | `secondary_foreground`   | current            |
| `ghost`               | transparent                                                                     | `accent`                                                  | `foreground`             | `muted_foreground` |
| `ghost-muted`         | transparent                                                                     | `accent`, text → `foreground`                             | `muted_foreground`       | current            |
| `ghost-destructive`   | transparent                                                                     | `accent`, text → `destructive`                            | `muted_foreground`       | current            |
| `glass`               | `glass_surface` over blur, `border` at 60%, full radius, pill shadow            | border `border`                                           | `foreground`             | `muted_foreground` |
| `warning-outline`     | border `warning` at 32%, fill `warning_surface`                                 | border `warning` at 40%, fill `warning` at 16% (dark 24%) | `warning_foreground`     | current            |
| `link`                | transparent                                                                     | underline, offset 4                                       | inherit                  |                    |

`overlay`, `media-close` and `media-navigation` exist only for the media viewer (black/white alphas).
`InlineButton` is a text-only button (tones: default, muted, destructive, picker) that underlines on hover.

### 6.2 Inputs (`input.tsx`, `textarea.tsx`, `select.tsx`)

| Part                             | Spec                                                                                                                                                                                               |
| -------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Input box                        | Radius 8, 1px `input` border, fill `background` (dark: `input` at 32%), 14px text, control shadow and bevel                                                                                        |
| Input heights (including border) | default 32 (inner 30, pad-x 11), `sm` 28 (pad 9), `lg` 36, `compact` 30 (radius 6, 12px text, pad 9)                                                                                               |
| Placeholder                      | `placeholder`                                                                                                                                                                                      |
| Textarea                         | Same box; min 70, max 256 then scrolls; pad 11 / 5 (`sm`: min 66, pad 9 / 3; `lg`: min 74, pad-y 7). Grows with content                                                                            |
| Select trigger                   | Same box as an input. Min height 32 (`sm` 28, `lg` 36, `xs` 24, `compact` 28; xs and compact radius 6 with 12px text). Gap 8. Trailing 14px chevron-down at 0.7. Leading icons `icon_muted` at 0.8 |
| Select `ghost` trigger           | Transparent; text `secondary_label`; hover `accent` with `foreground` text                                                                                                                         |
| Select popup                     | `dropdown_surface` over blur, radius 8, floating shadow, list padding 4, at least the trigger width, offset 4                                                                                      |
| Select item                      | Min 28, pad 8 / 4, radius 4, 14px text. Highlighted `accent` with `accent_foreground`; selected `foreground` at 8%                                                                                 |

### 6.3 Menu (`menu.tsx`, also the model for context menus)

| Part                  | Spec                                                                                                                                                                             |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Popup                 | `dropdown_surface` with 1px `dropdown_border`, over blur. Radius 8, floating shadow, min width 160, inner padding 4, offset 4, layer 130                                         |
| Item                  | Min 28, pad 8 / 4, gap 8, radius 4, 14px text `foreground`. Icon 16 at 0.8 opacity in `muted_foreground`. Highlighted `accent` with `accent_foreground`. Inset items start at 32 |
| Destructive item      | Text `destructive_foreground`; icon uses the text color                                                                                                                          |
| Checkbox / radio item | Checkbox: 16px check column, end padding 16, check glyph stroke 1.75. Radio checked fill `foreground` at 8%                                                                      |
| Switch item           | Embedded mini switch, thumb 12                                                                                                                                                   |
| Group label           | Pad 8 / 6, 12px, 500 weight, `muted_foreground`                                                                                                                                  |
| Separator             | 1px `border`, margin 4 / 8                                                                                                                                                       |
| Shortcut              | End-aligned, 12px, 500 weight, tracking 0.1em, `secondary_label`                                                                                                                 |
| Submenu               | Trigger ends in a chevron-right; opens inline-end, align offset −5                                                                                                               |
| Search field          | Pad 8 / 6 / 4; underline 1px `border` at 70%, `ring` on focus; input 26px with 20px start pad; search icon 16 in `muted_foreground` at 55%                                       |

Desktop context menus (`api.contextMenu.show`) are native OS menus in Electron. Native GPUI renders
them with this Menu spec, anchored at the pointer.

### 6.4 Dialog (`dialog.tsx`, `dialog-styles.ts`)

| Part     | Spec                                                                                                                                           |
| -------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Backdrop | `dialog_backdrop` plus 4px blur                                                                                                                |
| Popup    | `dialog_surface` over blur (§4.6), 1px `dialog_border`, radius 16, dialog shadow, max width 512                                                |
| Header   | Padding 24, gap 8. Title 20px, 600 weight, line height 1. Description 14px `muted_foreground`                                                  |
| Panel    | Padding 24 (top 4 after a header), 16 between children, scroll fades                                                                           |
| Footer   | Padding 16 / 24, gap 8, buttons end-aligned. Top 1px `border`, fill `muted` at 72%, bottom radius 15. The `bare` variant has no border or fill |
| Close    | Ghost icon button (32) with a 16px X, 8px from top and end                                                                                     |
| Nested   | Each level: scale −0.1, translate up 20px, opacity −0.1                                                                                        |

### 6.5 Popover and tooltip (`popover.tsx`, `tooltip.tsx`)

| Part            | Popover                                          | Tooltip                                                                                    |
| --------------- | ------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| Surface         | `dropdown_surface`, `dropdown_border`, over blur | `popover` with 1px `border` (the `glass` variant uses dropdown glass and the toast shadow) |
| Radius / shadow | 8 / floating, plus control bevel                 | 6 / tooltip shadow, plus control bevel                                                     |
| Padding         | default 16 all sides; compact 8 / 12; none       | 8 / 4                                                                                      |
| Width           | auto, 256, 320, 384                              | max 320 (`code` variant: max 480, mono 11px, line height 1.625)                            |
| Text            | 14 (title 14, 600 weight, line height 1)         | 12, line height 1.375, balanced wrap                                                       |
| Offset / side   | 4 / bottom                                       | 4 / top                                                                                    |
| Open delay      | on click                                         | 600ms (Base UI default). The sidebar uses 150ms, settings 200ms                            |

### 6.6 Toast (`toast.tsx`)

Top-right stack, max width 360. Surface: dropdown glass, radius 8, toast shadow. Content: 14px start
padding, 12px vertical, 14px text. Title 500 weight; description `muted_foreground`. Type icon 16px in
`destructive`, `info`, `success` or `warning`. Stacked toasts sit 12px apart, scale down 10% per
index, and peek 12px. Close orb: 24px circle, `border` at 60%, `popover` at 92%, pill shadow, 12px X
with stroke 2.25. The action is an `xs` button.

### 6.7 Toggles and selection controls

| Control                                      | Spec                                                                                                                                                                                                                                                                                                         |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Switch (`switch.tsx`)                        | Thumb 16 (`sm`: 14); track = thumb + 2 tall, thumb × 2 − 2 wide (18×30), 2px padding, full radius. On `primary`, off `input`. Thumb `background` (dark off: `muted_foreground`) with shadow-sm at 5%, travels thumb − 4px. Pressed: thumb widens to 1.1×                                                     |
| Checkbox (`checkbox.tsx`)                    | 16px, radius 4, 1px `input` border, fill `background` (dark `input` at 32%), control shadow. Checked: `primary` fill, 12px check in `primary_foreground`, stroke 3. Focus: 2px `ring` outline inside                                                                                                         |
| Toggle (`toggle.tsx`)                        | Radius 8, transparent border, 500 weight, 14px text, icons at 0.8. Hover `accent`; pressed `input` at 64% with `accent_foreground`. Sizes (square min): default 32 (pad 7), `sm` 28 (pad 5), `xs` 24 (radius 6, pad 3), `lg` 36, `compact` 28 (radius 6, 12px), `segmented` 24 (pad 10, radius 6, 12px)      |
| Toggle variants                              | `ghost` (pressed `accent`); `outline` (input box look); `pill` (full radius, pad 12, 400 weight, `muted_foreground` → `foreground` when pressed); `segmented` (`muted_foreground`; hover `background` at 55%; pressed `background` with shadow-xs at 10%; dark hover `input` at 32%, pressed `input` at 72%) |
| ToggleGroup `segmented` (`toggle-group.tsx`) | Track `input` at 40%, radius 8, 2px padding, 2px gap. This is the app's tab control: there is no Tabs primitive                                                                                                                                                                                              |
| Radio (pending-input panel)                  | 16px circle, `input` border; checked `primary` with a 6px dot                                                                                                                                                                                                                                                |

### 6.8 Small primitives

| Primitive                                       | Spec                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Badge (`badge.tsx`)                             | Radius 4, transparent border, 500 weight, gap 4, 12px icons at 0.8. Sizes: default 18 tall (min width 18, pad 3, 12px); `sm` 16 (pad 3, 10px, line height 1); `lg` 22 (pad 5, 14px); `control` 24 (radius 8, pad 7, 12px)                                                                                                                                                                                                                                                          |
| Badge variants                                  | `default` `primary` fill. `destructive` fill with white text. `error`, `info`, `success`, `warning`: the color at 8% (dark 16%) with `*_foreground` text. `outline`: `input` border, `background` fill (dark `input` at 32%). `secondary`. `label`: custom color at 8% (dark 12%), text mixed 30% (dark 45%) into `foreground`                                                                                                                                                     |
| Kbd (`kbd.tsx`)                                 | 20 tall, min width 20, radius 4, pad-x 4, `foreground` at 7%, 12px, 500 weight, `muted_foreground`, 12px icons. Groups gap 4                                                                                                                                                                                                                                                                                                                                                       |
| Alert (`alert.tsx`)                             | Radius 12, 1px border, pad 14 / 12, 14px `card_foreground`, 16px icon, gap 8. Title 500 weight; description `muted_foreground`. Variants: `default` (dark: `input` at 32%); `error` / `warning` (border at 32%, `*_surface`, `*_foreground` text, description at 80%); `info` / `success` (border at 32%, fill at 4%); `sidebar` (radius 8, `sidebar_border`, `sidebar_control_surface`, pad 8 / 6, 11px on a 16px line). The `glass` surface tints a 4% wash over `glass_surface` |
| Scroll area (`scroll-area.tsx`)                 | Overlay scrollbar 6px wide, 4px end inset, 1px off the edge. Full-radius thumb `scrollbar_thumb` (hover `scrollbar_thumb_hover`). Hidden until hover or scroll (fade in 100ms, fade out after 300ms). Optional 24px edge fades                                                                                                                                                                                                                                                     |
| Separator                                       | 1px `border`                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| Skeleton (`skeleton.tsx`)                       | `muted_foreground` at 15%. Radius 4 (block), 8 (card), full (pill). Stepped pulse (§9)                                                                                                                                                                                                                                                                                                                                                                                             |
| Spinner (`spinner.tsx`)                         | Lucide `loader-circle`; sizes 12 / 14 / 16 / 20; tone current or `muted_foreground`. One turn per second, only while visible (§9)                                                                                                                                                                                                                                                                                                                                                  |
| Sidebar menu button (`sidebar.tsx:714`)         | 32 tall, radius 8, pad 10 / 6, gap 8, 14px, 500 weight, `sidebar_muted_foreground` at 80%. Icon 16 in `sidebar_icon`. Hover `sidebar_row_hover`; pressed `sidebar_row_active`; selected `sidebar_row_selected` with `sidebar_foreground` (icon too). Sizes `sm` 28 (12px), `lg` 48, `icon` 32 square                                                                                                                                                                               |
| Command row (`autocomplete.tsx`, `command.tsx`) | Min 28, pad 8 / 6, radius 4, 14px. Highlighted `foreground` at 9%; selected 6%. Group label 12px, 500 weight, `muted_foreground`. Input row pad 8 / 6, input 34px; list padding 8; footer pad 16 / 10 with `border-t`                                                                                                                                                                                                                                                              |

## 7. App compositions

Sources: `components/Sidebar.tsx`, `components/sidebar/*`, `components/Sidebar.logic.ts`,
`components/ThreadStatusIndicators.tsx`, `components/chat/*`, `components/ChatMarkdown.tsx`,
`components/ChatView.tsx`, `lib/diffRendering.ts`, `components/diffs/StyledDiffCodeView.tsx`.

### 7.1 Sidebar chrome

| Part                                         | Spec                                                                                                                                   |
| -------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Surface                                      | `sidebar`; edge 1px `sidebar_edge`                                                                                                     |
| Header row                                   | 52px, end padding 12, gap 8, drag region. Brand: 28px box; "Code" 14px, 500 weight, tracking −0.025em, `muted_foreground`              |
| Search field                                 | 32 tall, radius 6, pad 8 / 6, gap 8, 14px, 500 weight, `sidebar_muted_foreground`; 16px icon `sidebar_icon`; hover `sidebar_row_hover` |
| Header buttons                               | 28px icon buttons: Filter by project, Add project, New thread                                                                          |
| Section header (Working / Snoozed / Settled) | 32 tall, pad-x 8, radius 6, 12px, 500 weight, `sidebar_muted_foreground`, trailing count, 12px chevron                                 |
| Footer                                       | Pad 8 / 4 / 8, gap 8; a row of 32px icon buttons (Settings, Pull requests, Usage)                                                      |
| Show more (Settled)                          | First 10, then 25 per page; 36px button, pad-x 10, 14px text                                                                           |

### 7.2 Sidebar thread row (`SidebarThreadRow.tsx`)

| Part                               | Spec                                                                                                                                                                                                                                                                                                         |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| List                               | 1px gap; each item has 2px vertical padding                                                                                                                                                                                                                                                                  |
| Card row (pinned, active, working) | 78px tall (auto when it has no meta line), pad 10 / 8, radius 6. Line 1 (20px, gap 6): 16px favicon, 12px pin, project name 12px `sidebar_secondary_label`, then status or time at the end. Line 2 (+4px): title. Line 3 (+2px, 12px `sidebar_secondary_label`, gap 6): branch, terminal, PR, provider icons |
| Slim row (snoozed, settled)        | 36px, pad-x 10, gap 10. Favicon at 0.4 opacity and grayscale until hover                                                                                                                                                                                                                                     |
| States                             | Current: `sidebar_row_active` plus, in light only, a 1px inset ring of `sidebar_border` at 70%. Multi-selected: `sidebar_row_selected` with the same ring. Hover: `sidebar_row_hover`. Unsent draft: `warning` at 4% (hover 8%). Rows have no pressed state                                                  |
| Title                              | 14px; 500 weight (400 when receding). Card: `foreground` (receding 75%, failed 95%). Slim: active/input/request `foreground`, unread `muted_foreground`, read `sidebar_secondary_label` at 70%                                                                                                               |
| Receding working rows              | Opacity 0.7, 1.0 on hover                                                                                                                                                                                                                                                                                    |
| Time                               | 12px, tabular, `sidebar_secondary_label`, compact ("now", "5m")                                                                                                                                                                                                                                              |
| Hover actions                      | Time fades out and actions fade in: 12px text `muted_foreground`, pad-x 6. Card: Discard draft (14px X), Snooze (12px clock), Settle (14px check + "Settle")                                                                                                                                                 |
| Drag                               | `sidebar` fill, `shadow-lg`, raised. Drop badge: 20px, radius 4, `primary` border at 40%, `primary` fill at 10%, 11px, 500 weight, `primary` text                                                                                                                                                            |

### 7.3 Thread status

Row label: icon plus 12px, 500-weight label. Icon 16px in card rows, 12px in folder rows. No animation.
Mapping: `resolveSidebarThreadStatus` (`Sidebar.logic.ts:882`).

| Status                           | Lucide icon               | Token                |
| -------------------------------- | ------------------------- | -------------------- |
| Working / Goal (+ live duration) | `circle-dashed`           | `info_foreground`    |
| Waiting (+ duration)             | `circle-dot-dashed`       | `muted_foreground`   |
| Approval                         | `shield-question`         | `warning_foreground` |
| Input                            | `message-circle-question` | `status_input_icon`  |
| Request (guest)                  | `user-round`              | `guest_foreground`   |
| Limited                          | `circle-alert`            | `warning_foreground` |
| Failed                           | `circle-alert`            | `error_foreground`   |
| Woke (clickable)                 | `alarm-clock`             | `warning_foreground` |
| Done, unread                     | `circle-check`            | `success_foreground` |

The command palette uses pills (`resolveThreadStatusPill`, `Sidebar.logic.ts:1109`): a 6px dot (9px
compact) plus a 10px label. Pending Approval `pill_approval`, Awaiting Input `pill_input`, Guest
Request `guest_foreground` / `guest`, Working and Connecting `pill_working` (pulsing), Waiting
`sidebar_muted_foreground`, Failed `pill_failed`, Plan Ready `pill_plan`, Completed `pill_completed`.
A running terminal shows a 14px terminal icon in `terminal_running`, pulsing. Folder dots use
`warning`, `error`, `success` or `info`.

### 7.4 Chat header (`ChatHeader.tsx`, `PanelLayoutControls.tsx`)

52px. Breadcrumb: 14px project favicon, project name (max 160, `muted_foreground`), a `/` in
`icon_muted`, then the title (14px, 500 weight, `foreground`, truncated; a 14px chevron on hover).
Gap 12. The right cluster is 12px from the edge with 4px gaps: Maximize (only while a panel is open),
Thread details, Terminal, Right panel. Each is a ghost Toggle, `sm` 28px, 16px icon at 0.8; hover and
pressed use `accent` (`toolbar_control_hover` under named themes).

### 7.5 User message (`TimelineUserMessageRow.tsx`)

| Part          | Spec                                                                                                                                                                                               |
| ------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Bubble        | End-aligned, max 80% of the lane, radius 16, padding 12, `message_surface` (guest messages `guest_surface`), 14px on a 22.75px line, `message_foreground`. Corners on the grouped side shrink to 6 |
| Long messages | Collapsed to 176px with a 28px fade and a "Show full message" `xs` ghost-muted button                                                                                                              |
| Images        | Inline grid: 2 columns, gap 8, max 210, 4:3, radius 8. A lone image is 192 tall, radius 16, with a 1px inset ring (black 9%, dark white 12%)                                                       |
| Actions       | Below the bubble, end-aligned, hidden until row hover (200ms fade): timestamp (12px tabular `muted_foreground`), then Edit, Copy and Copy link as `xs` ghost buttons with 12px icons               |

### 7.6 Assistant message (`TimelineAssistantRows.tsx`, `ChatMarkdown.tsx`, `index.css:1868–2271`)

| Part         | Spec                                                                                                               |
| ------------ | ------------------------------------------------------------------------------------------------------------------ |
| Prose        | Pad 4 / 2. 14px on a 22.75px line, `foreground` at 90%, no tracking                                                |
| Block rhythm | Paragraphs, lists, quotes, code and tables: 10.4px vertical margin. First and last child have no outer margin      |
| Lists        | Indent 20px; 4px between items. Disc → circle → square; decimal → lower-alpha → lower-roman                        |
| Blockquote   | 2px start border in the text color at 35%, start padding 12.8px                                                    |
| Links        | `info_foreground`, no underline; dotted underline on hover                                                         |
| Tables       | 14px; cells pad 7.2 / 12, the outer cells flush with the prose; header 600 weight; rows divided by `border` at 60% |
| Footer       | +6px, hover-only: Copy (`xs` ghost-muted), Fork (`xs` ghost), timestamp                                            |
| Streaming    | Each new block fades in over 600ms (opacity only, once)                                                            |
| Turn spacing | 16px below a turn; 4px when grouped; 8px for assistant rows without meta                                           |

### 7.7 Work, tool and reasoning rows (`WorkLog.tsx`, `TimelineWorkEntryRow.tsx`)

| Part                           | Spec                                                                                                                                                                                           |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Row                            | Min 24, gap 6, 14px on a 1.625 line, radius 6, pad-x 2 (2px vertical when standalone)                                                                                                          |
| Icon                           | 24px slot, 16px icon, stroke 2, `icon_muted`                                                                                                                                                   |
| Label                          | `secondary_label`, truncated (non-tool entries `foreground` at 80%)                                                                                                                            |
| Trailing                       | Timestamp on hover; 12px chevron `icon_muted` at 0.7, rotated 90° when open                                                                                                                    |
| Expanded body                  | Max 256 then scrolls; mono at code size on a 1.625 line, `secondary_label`; margins 2 / 6                                                                                                      |
| Command output                 | Streamed (`CommandOutputPanel.tsx`): the body is not capped; the output scrolls at max 320, pinned to the tail while the reader is within 8px of the bottom, `muted_foreground`                |
| Group summary ("N tool calls") | Same row with summary icon and label; expanded list max `min(288px, 50% of window height)`, rows 24                                                                                            |
| Error                          | Icon `tool_error_icon` at 40%. Severe: 16px `circle-alert` and a 14px, 500-weight label in `destructive`; message indented 28px in `foreground` at 80%                                         |
| Running                        | Label shimmer: a 72px highlight sweeps across the text, 2.2s per pass in 30 steps. Reasoning: brain icon, the same sweep, live preview clamped to 4 lines; expanded trace max 384, indented 28 |
| Approval request               | A work row. Detail mono 12px on a 20px line, max 160. "Waiting for approval" in `warning` with a 12px shield. Approve (default tone), Decline (muted), More                                    |

### 7.8 Code blocks and diffs

| Part                                          | Spec                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Code block frame (`MarkdownCodeBlock.tsx:68`) | Vertical margin 10.4, radius 8, 1px `code_block_border`, `code_block_background`, line height 1.375                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Code block header                             | Pad 6 / 6 / 0 / 12 (about 30px tall). Language label mono 11px at 72% `foreground` (or a 14px file icon). End: Wrap, Run (shell only), Copy as `icon-xs` buttons (24) with 12px icons, gap 2. Wrap pressed = `secondary` variant                                                                                                                                                                                                                                                                                                                                                                                                            |
| Code block body                               | Pad 12.8 / 14.4, mono at code size, transparent fill; `pre-wrap` when wrapping                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Changed-files card (`ChangedFilesTree.tsx`)   | Margin-top 16, radius 8, `secondary` fill (dark `input` at 20%). Sticky header pad 12 / 8, 12px, 500 weight. Tree padding 8; rows 28 tall, gap 8, radius 6, start padding 8 + 14 × depth, hover `accent` at 60%. Folder name mono 11px; file name mono 12px `foreground` at 85%; stats mono 10px in a `4ch 4ch` grid                                                                                                                                                                                                                                                                                                                        |
| Diff panel (`StyledDiffCodeView.tsx`)         | Mono 13px on a 20px line. File header 32px, 12px sans, pad 8 / 12, sticky. Hunk separator 24px, 11px text                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Diff row fills (`diffRendering.ts:267–330`)   | `diff_row_addition` / `diff_row_addition_gutter` and the `deletion` pair: Pierre mixes the surface (88%, gutter 91%; dark 80% / 85%) `in lab` toward the overrides `diffRendering.ts` sets (surface 50% / 35% into `diff_addition`; dark 70% / 60%). The diff viewer sets `--code-background: var(--background)` (`StyledDiffCodeView.tsx`), so the surface is `background`: context rows, headers and the list paint it, and `diff_hunk_separator` (97% toward `code_foreground`) fills separators. Gutter numbers: `diff_*_foreground`, context `code_foreground` at 65%. A 4px `diff_addition` / `diff_deletion` bar marks changed lines |
| Diff icons                                    | Added `success_foreground` (dark); deleted `destructive`; modified `diff_modified_icon`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |

### 7.9 Composer (`ComposerSurface.tsx`, `ChatComposer.tsx`, `ComposerPrimaryActions.tsx`)

| Part                                    | Spec                                                                                                                                                                                                   |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Shell                                   | Lane width, radius 16. Glass fill `card` (light) or `surface_raised` (dark) at `GLASS_OPACITY` over blur; composer shadow and outline (§4.3)                                                           |
| Body                                    | Pad 16 / 16 / 8 (x / top / bottom)                                                                                                                                                                     |
| Editor                                  | Composer font at prompt size, line 1.625. Min height two lines + 8px (about 53.5px), max 208 then scrolls. Placeholder `placeholder`: "Ask anything. @ files, $ skills, / commands"                    |
| Footer                                  | Pad 8 (x and bottom). Attach: `icon-sm` ghost (28) with a paperclip, 8px end margin. Controls gap 2, then 8 before Send                                                                                |
| Footer control                          | 28 tall, pad-x 8, gap 6, 500 weight, `foreground` at 80%, radius 8, 16px icons, hover `accent` (compact: 24 tall, pad 7, 12px)                                                                         |
| Send                                    | 32px circle, `message_action`, 14px up-arrow with stroke 1.8, `message_action_foreground`. Hover scales to 1.05 and uses `message_action_hover`. Disabled: `foreground` at 12% with `muted_foreground` |
| Stop                                    | 32px circle, `destructive` at 90%, white 8px rounded square (corner 1.5)                                                                                                                               |
| Text pills (Refine, Implement, answers) | 32 tall, full radius, pad-x 16                                                                                                                                                                         |
| Attached banners (`ComposerBanner.tsx`) | Inset 22px per side, overlap the card by 17px, top radius 16, pad 4, text 12px on a 16px line. Icon column 24; scroll max `min(384px, 40% of window height)`                                           |
| Context strip                           | Below the card, pad 8 / 8 / 0, `muted_foreground`                                                                                                                                                      |

### 7.10 Model picker (`ProviderModelPicker.tsx`, `ModelPickerContent.tsx`, `ModelListRow.tsx`)

Trigger: footer control, min width 52, 16px provider icon, label truncated at 224 with the effort
suffix after it. Popup: popover surface; content max 360 wide and a fixed 346 tall. Effort column 192
wide with `border-l` and `muted` at 40%. Provider rail 44 wide on `muted` at 30%, with square buttons
at radius 6. Search: pad 12 / 10, underline `border` at 70%, input 26px with 20px start pad, 16px
icon at 55%. Rows: min 28, pad 8 / 4, radius 4, highlight `foreground` at 9%, 16px icon, name 14px on
a 1.375 line, sub-provider 12px `muted_foreground` at 70%, "New" badge 10px bold uppercase, favorite
star `icon-xs`, and a 14px check slot that is always reserved.

### 7.11 Other surfaces

| Surface                              | Spec                                                                                                                                                                                                                                                   |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Scroll-to-bottom                     | `xs` glass pill, 14px chevron-down plus summary in `muted_foreground`, 4px above the composer                                                                                                                                                          |
| Empty thread                         | "Send a message to start the conversation.", 14px `muted_foreground`, centered                                                                                                                                                                         |
| New-thread hero                      | Composer vertically centered; headline 30px, 400 weight, tracking −0.025em, centered, 16px above the composer                                                                                                                                          |
| Settings group (`SettingsGroup.tsx`) | Radius 12, 1px `border` at 60%, `card` at 40%, control shadow; row dividers `border` at 50%. Rows pad 16 / 12; title 14px 500 weight; description 12px. A two-column grid `[1fr minmax(160px, auto)]` with gap 32 once the container is at least 512px |
| Thread details card                  | Section header min 32, pad-x 10, 11px, 500 weight, `muted_foreground`. Rows 32, radius 8, pad-x 10, 13px, 500 weight, `foreground` at 80%                                                                                                              |

## 8. Iconography

Sources: `apps/web/package.json` (`lucide-react ^0.564.0`), `index.css:1726–1732`, `components/Icons.tsx`.

| Rule           | Value                                                                                                                                                                            |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Set            | Lucide; 163 distinct icons imported by the web. 24×24 viewBox, stroke `currentColor`, round caps and joins, no fill                                                              |
| Default stroke | **1.75**. The web overrides Lucide's 2 globally. Explicit exceptions: work-row icons 2, send arrow 1.8, toast close 2.25, checkbox glyph 3                                       |
| Sizes          | 12 (micro and xs buttons, chevrons, badges, kbd), 14 (compact/xs controls, menu check, code-block icons), **16 (default everywhere)**, 18 (`xl` buttons), 20 (`lg` spinner)      |
| Color          | Inherits text color, except: ghost and outline buttons use `muted_foreground`; menu items `muted_foreground` at 0.8; select triggers `icon_muted` at 0.8; sidebar `sidebar_icon` |
| Brand marks    | Provider and editor logos are multi-color SVG components (`Icons.tsx`, `JetBrainsIcons.tsx`); project favicons are images                                                        |

Native shipping:

1. Vendor only the icons you use as upstream Lucide SVGs, pinned to the web's Lucide version, under
   `apps/native/crates/app/assets/icons/lucide/<name>.svg` (ISC license file alongside). Bake
   `stroke-width="1.75"` in when vendoring, because GPUI `svg()` renders a one-color mask and cannot
   change stroke width at runtime. Add a separate file only where the web uses another stroke.
2. Serve them through one `AssetSource` (`include_bytes!` table) and expose a typed `Icon` enum. Views
   write `Icon::CircleDashed`, never a path string.
3. Render monochrome icons with `svg().path(..).size(px(16.)).text_color(token)`. Render multi-color
   brand logos with `img()` so their fills survive.

## 9. Motion

Sources: `index.css:191–362, 551–660, 2203–2218`, Tailwind `theme.css`, primitives, `lib/visibleAnimation.ts`,
`packages/contracts/src/settings.ts` (`panelAnimationDurationMs`).

| Motion                              | Duration / easing                                                                                                                     |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| Default transition (`transition-*`) | 150ms `cubic-bezier(0.4, 0, 0.2, 1)` (`DEFAULT_TRANSITION_*`)                                                                         |
| Button press                        | Scale 0.97 over 150ms                                                                                                                 |
| Dialog and backdrop                 | 200ms ease-in-out; popup scale 0.98 ↔ 1 plus opacity                                                                                  |
| Menus, popovers, tooltips, selects  | Scale 0.98 ↔ 1 plus opacity, 150ms. Tooltips open instantly while moving between triggers                                             |
| Switch                              | Track color 200ms; thumb translate 150ms; press stretch 100ms after 100ms                                                             |
| Scrollbar                           | Fade in 100ms; fade out after 300ms                                                                                                   |
| Toast                               | Transform 500ms `cubic-bezier(0.22, 1, 0.36, 1)`, opacity 500ms, height 150ms                                                         |
| Hover-revealed actions              | Opacity 200ms                                                                                                                         |
| Streaming markdown block            | Opacity 0 → 1 over 600ms ease-out, once per new block                                                                                 |
| Surfaces that slide in              | `EASE_DRAWER` `cubic-bezier(0.32, 0.72, 0, 1)`                                                                                        |
| Sidebar and panel width             | `--panel-animation-duration`, **default 0ms** (setting 0–400) because width transitions cost per-frame layout. Panels snap by default |
| Theme switch                        | No transitions at all                                                                                                                 |

Looping indicators on the web are duty-cycled and pause when offscreen, when the tab is hidden, or
under reduced motion:

| Loop                                         | Web definition                                                     | Native implementation                                          |
| -------------------------------------------- | ------------------------------------------------------------------ | -------------------------------------------------------------- |
| Skeleton                                     | 2.4s; opacity holds 1, ramps to 0.55 in 4 steps, holds, ramps back | Redraw only at the step boundaries (about 3.3 fps)             |
| Status pulse (working, connecting, terminal) | 2s; opacity holds 1, ramps to 0.5 in 6 steps                       | Same: step timer, never per-vsync                              |
| Status ping                                  | 2s, 8 steps, scale 0.75 → 2, fading out                            | Same                                                           |
| Spinner                                      | 1s linear rotation                                                 | Throttle to ≤30 fps (or 12 discrete angles per turn)           |
| Tool shimmer and focus sweep                 | 2.2s in 30 steps (about 13.6 fps)                                  | Same step count. Static `secondary_label` under reduced motion |

Hard rules for native:

- **An idle window draws nothing.** No timers, `request_animation_frame` loops or per-frame
  `cx.notify()` while nothing is animating.
- **No loop exceeds 30 fps.** Stepped animations request frames only at step boundaries.
- **Loops pause** when the element is not visible, the window is minimized or occluded, or the OS
  "Reduce motion" setting is on. Under reduced motion, spinners and sweeps render static.
- **Don't animate layout** (widths, heights, list reflow) by default. Animate opacity and transform only.

## 10. GPUI implementation rules

1. **One theme global.** `Theme { palette: tokens::Palette, appearance, ui_scale, … }` implements
   `gpui::Global` and is read as `Theme::of(cx)`. A theme change replaces the global and refreshes the
   windows. Views never cache colors.
2. **Semantic tokens only.** Views and components read `theme.palette.<token>` and convert with
   `gpui::rgba(u32)`. No `rgb(0x…)`, `rgba(0x…)`, `hsla(…)` or Tailwind hexes outside `theme/`. The
   only allowed transform at a call site mirrors the web's `/NN` (multiply a token's alpha). If a
   needed color has no token, add it to `gen-theme.ts` from the web source and regenerate.
3. **Components own their look** (the web's `shadcn/no-restyle`). Every primitive in §6 is a native
   component with `variant` and `size` builders: `Button::new("send").variant(Ghost).size(IconSm)`.
   Call sites never set fill, text color, border, radius, padding or height on a component. If no
   variant fits and the look is generic, add a variant to the component. If it belongs to one
   feature, it lives in that feature's component.
4. **Layout belongs to the parent.** Width, flex, margins, gaps and positioning are set by the
   container wrapping the component, never by restyling it.
5. **Use metrics, scaled correctly.** Take sizes from `tokens::metrics` or the tables here. Multiply
   `// rem` metrics by `ui_scale = interface_font_size / metrics::ROOT_FONT_SIZE` through one helper
   (`theme.rem(px)`); never scale `// px` metrics. Use the desktop (`sm:`) values.
6. **gpui-component is plumbing, not the look.** Its widgets read `gpui_component::Theme`. Sync that
   from our palette once per theme change so inputs and scrollbars match. Wrap any widget you use in
   our component; never restyle it at a call site.
7. **Respect scopes and alpha.** Sidebar subtrees use `sidebar_*` tokens. Translucent tokens composite
   and are never pre-flattened.
8. **Glass sparingly.** Use `window.paint_backdrop_blur` for dialogs, menus, popovers, toasts and the
   composer, with the opaque fallback token, and nowhere else.
9. **Performance.** Follow §9. Virtualize the sidebar and transcript (`list` / `uniform_list`). No
   allocation of static strings per frame.
10. **The generated file is read-only.** Change the web source, run `node apps/native/scripts/gen-theme.ts`,
    and commit both. `--check` fails CI when they drift.

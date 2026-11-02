//! Expected values come from running `packages/shared/src/terminalOutput.ts` on the same input;
//! `tests/conformance.rs` replays seeded fuzz cases against it as well.

use super::*;

fn fold(chunks: &[&str], max_chars: usize) -> TerminalOutputState {
    let mut state = TerminalOutputState::default();
    for chunk in chunks {
        state.append(chunk, max_chars);
    }
    state
}

fn text(chunks: &[&str]) -> String {
    fold(chunks, TERMINAL_OUTPUT_TAIL_CHARS).text
}

#[test]
fn carriage_returns_and_erases_redraw_a_line() {
    assert_eq!(text(&["hi\rH\n"]), "Hi\n");
    assert_eq!(text(&["10%\r20%\r\u{1b}[K30%\n"]), "30%\n");
}

#[test]
fn styles_reset_at_each_line_end_and_resume_on_the_next() {
    assert_eq!(text(&["\u{1b}[31mred\nstill\u{1b}[0m plain\n"]), "\u{1b}[0;31mred\u{1b}[0m\n\u{1b}[0;31mstill\u{1b}[0m plain\n");
}

#[test]
fn cursor_moves_reach_the_live_window() {
    assert_eq!(text(&["a\nb\nc\u{1b}[2A\u{1b}[2KX\n"]), " X\nb\nc");
    assert_eq!(text(&["one\u{1b}7\ntwo\u{1b}8X\n"]), "oneX\ntwo");
    assert_eq!(text(&["a\nb\u{1b}[2J c"]), " c");
    assert_eq!(text(&["a\nbc\u{1b}D\u{1b}[1J!"]), "\n  !");
}

#[test]
fn string_sequences_are_hidden_even_split_across_chunks() {
    assert_eq!(text(&["x\u{1b}]0;title\u{7}y\u{1b}]8;;http://e\u{1b}\\link\u{1b}]8;;\u{1b}\\z\n"]), "xylinkz\n");
    let state = fold(&["a\u{1b}]0;ti", "tle\u{7}b\u{1b}[3"], TERMINAL_OUTPUT_TAIL_CHARS);
    assert_eq!(state.text, "ab");
    assert_eq!(state.pending(), "\u{1b}[3");
    assert_eq!(text(&["a\u{1b}]0;ti", "tle\u{7}b\u{1b}[3", "1mc"]), "ab\u{1b}[0;31mc\u{1b}[0m");
}

#[test]
fn an_unfinished_escape_past_the_pending_limit_is_dropped() {
    let long = format!("ok\u{1b}[{}", "1;".repeat(40));
    let state = fold(&[&long], TERMINAL_OUTPUT_TAIL_CHARS);
    assert_eq!(state.pending(), "");
    // A long string payload keeps only its introducer, so its tail stays hidden.
    let osc = format!("ok\u{1b}]{}", "x".repeat(80));
    let state = fold(&[&osc], TERMINAL_OUTPUT_TAIL_CHARS);
    assert_eq!(state.pending(), "\u{1b}]");
    assert_eq!(text(&[&osc, "more\u{7}after"]), "okafter");
}

#[test]
fn output_past_the_budget_is_cut_and_marked_truncated() {
    let cases: [(&str, usize, &str); 4] = [
        // One long line keeps its end.
        ("0123456789abcdefghij", 8, "cdefghij"),
        // Scrolled-out lines go first.
        ("l1\nl2\nl3\nl4\nl5\n", 8, "l4\nl5\n"),
        // Lines below a cursor that moved up keep only what fits.
        ("aaaa\nbbbb\ncccc\u{1b}[2Ad", 6, "aad\nb\n"),
        // A cut never starts on half of a surrogate pair.
        ("ab😀cd", 4, "😀cd"),
    ];
    for (input, max_chars, expected) in cases {
        let state = fold(&[input], max_chars);
        assert_eq!((state.text.as_str(), state.truncated), (expected, true), "{input:?}");
    }
    assert_eq!(normalize_terminal_output("short\n", 8), ("short\n".to_owned(), false));
}

#[test]
fn spans_resolve_styles_against_the_palette() {
    let state = fold(
        &["\u{1b}[1;31mERR\u{1b}[0m ok \u{1b}[7mINV\u{1b}[27m \u{1b}[42;30m PASS \u{1b}[0m \u{1b}[38;5;196mx\u{1b}[38;5;245my\u{1b}[38;2;1;2;3mz\u{1b}[0m \u{1b}[2;3;4;9mdim\u{1b}[0m\n"],
        TERMINAL_OUTPUT_TAIL_CHARS,
    );
    let spans = terminal_output_spans(&state.text, &DARK_PALETTE, MAX_SPANS);
    let style = |color: Option<u32>, background: Option<u32>| Some(SpanStyle { color: color.map(hex), background: background.map(hex), ..SpanStyle::default() });
    let expected = [
        ("ERR", Some(SpanStyle { bold: true, ..style(Some(0xf47067), None).unwrap() })),
        (" ok ", None),
        // Inverse paints the default colour as the fill and keeps the text readable on it.
        ("INV", style(Some(0x1f2328), Some(0xadbac7))),
        (" ", None),
        (" PASS ", style(Some(0x3b4048), Some(0x57ab5a))),
        (" ", None),
        ("x", style(Some(0xff0000), None)),
        ("y", style(Some(0x8a8a8a), None)),
        ("z", style(Some(0x010203), None)),
        (" ", None),
        ("dim", Some(SpanStyle { dim: true, italic: true, underline: true, strike: true, ..SpanStyle::default() })),
        ("\n", None),
    ];
    let actual: Vec<(&str, Option<SpanStyle>)> = spans.iter().map(|span| (span.text.as_str(), span.style.clone())).collect();
    assert_eq!(actual, expected.to_vec());

    let state = fold(&["\u{1b}[38:2::10:20:30mc\u{1b}[0m \u{1b}[93;104mhi"], TERMINAL_OUTPUT_TAIL_CHARS);
    let spans = terminal_output_spans(&state.text, &DARK_PALETTE, MAX_SPANS);
    assert_eq!(spans[0].style, style(Some(0x0a141e), None), "colon form drops its colour-space id");
    assert_eq!(spans[2].style, style(Some(0x1f2328), Some(0x6cb6ff)), "low contrast text on a light fill turns dark");
}

#[test]
fn spans_past_the_cap_become_one_plain_remainder() {
    let state = fold(&["\u{1b}[31ma\u{1b}[32mb\u{1b}[33mc\u{1b}[0md"], TERMINAL_OUTPUT_TAIL_CHARS);
    let spans = terminal_output_spans(&state.text, &DARK_PALETTE, 2);
    let texts: Vec<&str> = spans.iter().map(|span| span.text.as_str()).collect();
    assert_eq!(texts, ["a", "b", "cd"]);
    assert_eq!(spans[2].style, None);
    assert_eq!(terminal_output_plain_text("\u{1b}[0;31mred\u{1b}[0m \u{1b}[x"), "red \u{1b}[x");
}

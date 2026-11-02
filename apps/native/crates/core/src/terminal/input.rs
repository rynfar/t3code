//! Keystrokes and pastes as PTY input. The editing shortcuts mirror `apps/web/src/keybindings.ts`
//! (`terminalNavigationShortcutData`, `terminalDeleteShortcutData`, `isTerminalClearShortcut`);
//! keys are encoded the way xterm does, which is what the web's ghostty surface sends.
//! Plain printable keys are left to the text input path, so IME composition works.

/// A keystroke in GPUI's vocabulary: `key` is the key name ("a", "left", "enter"), `key_char`
/// the character it types, `platform` is Cmd on macOS.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyInput<'a> {
    pub key: &'a str,
    pub key_char: Option<&'a str>,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
}

const WORD_BACKWARD: &str = "\x1bb";
const WORD_FORWARD: &str = "\x1bf";
const LINE_START: &str = "\x01";
const LINE_END: &str = "\x05";
const DELETE_TO_LINE_START: &str = "\x15";
const CLEAR: &str = "\x0c";
/// `terminal.write` caps `data` at 65,536 UTF-16 units; pastes are split well under it.
const MAX_CHUNK_UTF16: usize = 16_384;

/// The macOS editing shortcuts the drawer turns into readline input: Option-arrows move by
/// word, Cmd-arrows to the line ends, Cmd-Backspace deletes to the line start, and Cmd-K or
/// Ctrl-L clear the screen.
pub fn shortcut_data(key: &KeyInput) -> Option<&'static str> {
    let only = |control: bool, alt: bool, shift: bool, platform: bool| {
        key.control == control && key.alt == alt && key.shift == shift && key.platform == platform
    };
    match key.key {
        "l" if only(true, false, false, false) => Some(CLEAR),
        "k" if only(false, false, false, true) => Some(CLEAR),
        "backspace" if only(false, false, false, true) => Some(DELETE_TO_LINE_START),
        "left" | "right" => {
            let left = key.key == "left";
            if only(false, true, false, false) {
                Some(if left { WORD_BACKWARD } else { WORD_FORWARD })
            } else if only(false, false, false, true) {
                Some(if left { LINE_START } else { LINE_END })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Bytes for a key that is not plain text. `None` means the key is not the terminal's: a Cmd
/// shortcut for the app, or a printable character that arrives through text input (including
/// the characters macOS composes with Option).
pub fn encode_key(key: &KeyInput, app_cursor: bool) -> Option<Vec<u8>> {
    if key.platform {
        return None;
    }
    let modifier = 1 + u8::from(key.shift) + 2 * u8::from(key.alt) + 4 * u8::from(key.control);
    if let Some(sequence) = navigation_sequence(key.key, modifier, app_cursor) {
        return Some(sequence.into_bytes());
    }
    let with_alt = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if key.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        Some(out)
    };
    match key.key {
        "enter" => with_alt(b"\r"),
        "backspace" => with_alt(if key.control { &[0x08] } else { &[0x7f] }),
        "tab" => with_alt(if key.shift { b"\x1b[Z" } else { b"\t" }),
        "escape" => with_alt(&[0x1b]),
        "space" if key.control => with_alt(&[0x00]),
        name if key.control => control_byte(name).and_then(|byte| with_alt(&[byte])),
        _ => None,
    }
}

fn navigation_sequence(name: &str, modifier: u8, app_cursor: bool) -> Option<String> {
    let cursor = |final_byte: char| {
        Some(if modifier > 1 {
            format!("\x1b[1;{modifier}{final_byte}")
        } else if app_cursor {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[{final_byte}")
        })
    };
    let tilde = |code: u8| {
        Some(if modifier > 1 { format!("\x1b[{code};{modifier}~") } else { format!("\x1b[{code}~") })
    };
    let function = |final_byte: char| {
        Some(if modifier > 1 { format!("\x1b[1;{modifier}{final_byte}") } else { format!("\x1bO{final_byte}") })
    };
    match name {
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => function('P'),
        "f2" => function('Q'),
        "f3" => function('R'),
        "f4" => function('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ => None,
    }
}

/// Caret notation: Ctrl-A is 0x01, Ctrl-[ is ESC, and xterm's digit aliases.
fn control_byte(name: &str) -> Option<u8> {
    let mut chars = name.chars();
    let (c, rest) = (chars.next()?, chars.next());
    if rest.is_some() {
        return None;
    }
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' | '-' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Pasted text as PTY input: wrapped when the program asked for bracketed paste, with an
/// embedded end marker removed so a paste cannot break out of it; otherwise newlines become
/// carriage returns, as a typed Enter would send.
pub fn encode_paste(text: &str, bracketed: bool) -> String {
    let text = text.replace("\x1b[201~", "");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

/// Splits input into `terminal.write`-sized pieces on character boundaries.
pub fn paste_chunks(data: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (index, c) in data.char_indices() {
        if units + c.len_utf16() > MAX_CHUNK_UTF16 {
            chunks.push(&data[start..index]);
            start = index;
            units = 0;
        }
        units += c.len_utf16();
    }
    if start < data.len() {
        chunks.push(&data[start..]);
    }
    chunks
}

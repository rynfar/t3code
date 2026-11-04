//! Local terminal echo from `settings/SettingsFontPreviews.tsx`; never shell input.

pub const TERMINAL_PROMPT: &str = "\x1b[1;32m→\x1b[0m \x1b[1;36mt3code\x1b[0m \x1b[1;34mgit:(\x1b[1;31mmain\x1b[1;34m)\x1b[0m \x1b[1;33m✗\x1b[0m ";

#[derive(Default)]
pub struct LocalEcho {
    length: usize,
}

impl LocalEcho {
    pub fn input(&mut self, data: &str) -> String {
        match data {
            "\r" => {
                self.length = 0;
                format!("\r\n{TERMINAL_PROMPT}")
            }
            "\x7f" | "\x08" if self.length > 0 => {
                self.length -= 1;
                "\x08 \x08".into()
            }
            data if data.starts_with('\x1b') => String::new(),
            data => {
                let printable: String =
                    data.chars().filter(|c| *c >= ' ' && *c != '\x7f').collect();
                self.length += printable.chars().count();
                printable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echoes_only_printable_input_and_protects_the_prompt() {
        let mut echo = LocalEcho::default();
        assert_eq!(echo.input("\x7f"), "");
        assert_eq!(echo.input("aé\n\t\x00"), "aé");
        assert_eq!(echo.input("\x1b[D"), "");
        assert_eq!(echo.input("\x7f"), "\x08 \x08");
        assert_eq!(echo.input("\x08"), "\x08 \x08");
        assert_eq!(echo.input("\x08"), "");
        assert_eq!(echo.input("no command runs"), "no command runs");
        assert_eq!(echo.input("\r"), format!("\r\n{TERMINAL_PROMPT}"));
        assert_eq!(echo.input("\x7f"), "");
    }
}

//! LaTeX math detection (` ```math `, `$$...$$`, `\[...\]`, `$...$`, `\(...\)`), streaming-safe
//! closing checks, and readable Unicode rendering (`packages/shared/src/markdownMath.ts`,
//! `apps/web/src/components/chat/MarkdownMath.tsx`).

/// Rendered representation of a TeX expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathRender {
    pub tex: String,
    pub display_text: String,
    pub block: bool,
}

/// Quick check before scanning markdown for inline or block math delimiters.
pub fn might_contain_math(source: &str) -> bool {
    source.contains('$') || source.contains("\\[") || source.contains("\\(")
}

/// Rewrites closed block math (`$$...$$`, `\[...\]`) into `t3-math` fenced custom blocks and
/// converts inline math (`$...$`, `\(...\)`) into formatted Unicode expressions, skipping code
/// spans/fences and currency expressions like `$10 and $20`.
pub fn preprocess_math_for_display(source: &str) -> String {
    if !might_contain_math(source) {
        return source.to_owned();
    }

    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut fence: Option<(u8, usize)> = None;

    while i < bytes.len() {
        let line_start = i == 0 || bytes[i - 1] == b'\n';
        if let Some((fc, flen)) = fence {
            let line_end = source[i..].find('\n').map(|n| i + n + 1).unwrap_or(bytes.len());
            let line = &source[i..line_end];
            out.push_str(line);
            let trimmed = line.trim_start();
            let close_len = trimmed.bytes().take_while(|&b| b == fc).count();
            if close_len >= flen && trimmed[close_len..].trim().is_empty() {
                fence = None;
            }
            i = line_end;
            continue;
        }

        if line_start {
            let trimmed = source[i..].trim_start_matches(' ');
            let leading = source[i..].len() - trimmed.len();
            if leading <= 3
                && let Some(fc @ (b'`' | b'~')) = trimmed.as_bytes().first().copied()
            {
                let count = trimmed.bytes().take_while(|&b| b == fc).count();
                if count >= 3 {
                    fence = Some((fc, count));
                    let line_end = source[i..].find('\n').map(|n| i + n + 1).unwrap_or(bytes.len());
                    out.push_str(&source[i..line_end]);
                    i = line_end;
                    continue;
                }
            }
        }

        // Preserve inline code spans verbatim.
        if bytes[i] == b'`' {
            let ticks = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            let start = i + ticks;
            if let Some(close) = find_closing_ticks(&source[start..], ticks) {
                let end = start + close + ticks;
                out.push_str(&source[i..end]);
                i = end;
                continue;
            }
        }

        // Preserve escaped `\$`.
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push_str("\\$");
            i += 2;
            continue;
        }

        // Display block `$$...$$`.
        if source[i..].starts_with("$$") {
            let body_start = i + 2;
            if let Some(close_rel) = find_unescaped_delim(&source[body_start..], "$$") {
                let tex = source[body_start..body_start + close_rel].trim();
                let is_block = tex.contains('\n') || (line_start && is_only_ws_to_eol(&source[body_start + close_rel + 2..]));
                if !tex.is_empty() {
                    if is_block {
                        emit_math_block_fence(&mut out, tex);
                    } else {
                        out.push_str(&render_tex_unicode(tex));
                    }
                    i = body_start + close_rel + 2;
                    continue;
                }
            }
        }

        // Display block `\[...\]`.
        if source[i..].starts_with("\\[") {
            let body_start = i + 2;
            if let Some(close_rel) = source[body_start..].find("\\]") {
                let tex = source[body_start..body_start + close_rel].trim();
                if !tex.is_empty() {
                    emit_math_block_fence(&mut out, tex);
                    i = body_start + close_rel + 2;
                    continue;
                }
            }
        }

        // Inline `\(...\)`.
        if source[i..].starts_with("\\(") {
            let body_start = i + 2;
            if let Some(close_rel) = source[body_start..].find("\\)") {
                let tex = &source[body_start..body_start + close_rel];
                if !tex.contains('\n') && !tex.trim().is_empty() {
                    out.push_str(&render_tex_unicode(tex.trim()));
                    i = body_start + close_rel + 2;
                    continue;
                }
            }
        }

        // Inline `$...$` on a single line, avoiding currency like `$10 and $20`.
        if bytes[i] == b'$'
            && !source[i..].starts_with("$$")
            && looks_like_inline_math_open(source, i)
        {
            let body_start = i + 1;
            if let Some(close_rel) = find_inline_dollar_close(&source[body_start..]) {
                let tex = &source[body_start..body_start + close_rel];
                if looks_like_inline_math_body(tex) {
                    out.push_str(&render_tex_unicode(tex.trim()));
                    i = body_start + close_rel + 1;
                    continue;
                }
            }
        }

        let ch = source[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

fn emit_math_block_fence(out: &mut String, tex: &str) {
    let ticks = "`".repeat(max_backtick_run(tex).max(3) + 1);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&ticks);
    out.push_str("t3-math\n");
    out.push_str(tex);
    out.push('\n');
    out.push_str(&ticks);
    out.push('\n');
}

fn max_backtick_run(text: &str) -> usize {
    let (mut max_run, mut cur) = (0, 0);
    for b in text.bytes() {
        if b == b'`' {
            cur += 1;
            max_run = max_run.max(cur);
        } else {
            cur = 0;
        }
    }
    max_run
}

fn find_closing_ticks(rest: &str, ticks: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let count = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if count == ticks {
                return Some(i);
            }
            i += count;
        } else {
            i += 1;
        }
    }
    None
}

fn find_unescaped_delim(rest: &str, delim: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(pos) = rest[from..].find(delim) {
        let abs = from + pos;
        if abs == 0 || rest.as_bytes()[abs - 1] != b'\\' {
            return Some(abs);
        }
        from = abs + delim.len();
    }
    None
}

fn is_only_ws_to_eol(rest: &str) -> bool {
    let line = rest.lines().next().unwrap_or("");
    line.trim().is_empty()
}

fn looks_like_inline_math_open(source: &str, dollar_idx: usize) -> bool {
    if dollar_idx > 0 {
        let prev = source[..dollar_idx].chars().next_back().unwrap_or(' ');
        if prev.is_alphanumeric() {
            return false;
        }
    }
    let after = &source[dollar_idx + 1..];
    after.chars().next().is_some_and(|c| !c.is_whitespace() && c != '$')
}

fn find_inline_dollar_close(rest: &str) -> Option<usize> {
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let line = &rest[..line_end];
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'$' {
            if i == 0 || bytes.get(i + 1) == Some(&b'$') {
                return None;
            }
            let prev = line[..i].chars().next_back().unwrap_or(' ');
            let next = line[i + 1..].chars().next();
            if !prev.is_whitespace() && next.is_none_or(|c| !c.is_ascii_digit()) {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn looks_like_inline_math_body(body: &str) -> bool {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Reject obvious currency ranges such as `$10 and $20` or `$5.00 - $9.50`.
    let first = trimmed.chars().next().unwrap();
    if first.is_ascii_digit()
        && !trimmed.contains(['\\', '^', '_', '=', '+', '*', '/', '<', '>'])
    {
        return false;
    }
    if trimmed.contains(" and ") && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    true
}

/// Converts a LaTeX math expression into a clean Unicode mathematical representation.
pub fn render_tex_unicode(tex: &str) -> String {
    let mut p = TexParser { src: tex.trim(), pos: 0 };
    p.parse_sequence(None)
}

struct TexParser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> TexParser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn parse_sequence(&mut self, stop: Option<char>) -> String {
        let mut out = String::new();
        while let Some(ch) = self.peek() {
            if Some(ch) == stop {
                self.bump();
                break;
            }
            match ch {
                '{' => {
                    self.bump();
                    out.push_str(&self.parse_sequence(Some('}')));
                }
                '^' => {
                    self.bump();
                    let arg = self.parse_script_arg();
                    out.push_str(&format_superscript(&arg));
                }
                '_' => {
                    self.bump();
                    let arg = self.parse_script_arg();
                    out.push_str(&format_subscript(&arg));
                }
                '\\' => {
                    self.bump();
                    out.push_str(&self.parse_command());
                }
                c if c.is_whitespace() => {
                    self.skip_ws();
                    if !out.ends_with(' ') && !out.is_empty() && self.peek().is_some_and(|n| Some(n) != stop) {
                        out.push(' ');
                    }
                }
                '-' if out.is_empty() || out.ends_with(['(', '[', '=', ' ']) => {
                    self.bump();
                    out.push('−');
                }
                _ => {
                    self.bump();
                    out.push(ch);
                }
            }
        }
        out.trim().to_owned()
    }

    fn parse_script_arg(&mut self) -> String {
        self.skip_ws();
        match self.peek() {
            Some('{') => {
                self.bump();
                self.parse_sequence(Some('}'))
            }
            Some('\\') => {
                self.bump();
                self.parse_command()
            }
            Some(ch) => {
                self.bump();
                ch.to_string()
            }
            None => String::new(),
        }
    }

    fn parse_group_or_atom(&mut self) -> String {
        self.parse_script_arg()
    }

    fn parse_command(&mut self) -> String {
        let Some(first) = self.peek() else { return String::new() };
        if !first.is_ascii_alphabetic() {
            self.bump();
            return match first {
                ',' | ';' | ':' | ' ' => " ".to_owned(),
                '!' => String::new(),
                '\\' => "\n".to_owned(),
                '{' | '}' | '$' | '%' | '#' | '&' | '_' | '|' => first.to_string(),
                _ => first.to_string(),
            };
        }
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.bump();
        }
        let cmd = &self.src[start..self.pos];
        match cmd {
            "left" | "right" | "big" | "Big" | "bigg" | "Bigg" => {
                self.skip_ws();
                if self.peek() == Some('.') {
                    self.bump();
                }
                String::new()
            }
            "frac" | "dfrac" | "tfrac" => {
                let num = self.parse_group_or_atom();
                let den = self.parse_group_or_atom();
                let num_s = if num.contains([' ', '+', '-', '−']) { format!("({num})") } else { num };
                let den_s = if den.contains([' ', '+', '-', '−']) { format!("({den})") } else { den };
                format!("{num_s}/{den_s}")
            }
            "sqrt" => {
                self.skip_ws();
                if self.peek() == Some('[') {
                    self.bump();
                    let _deg = self.parse_sequence(Some(']'));
                }
                let inner = self.parse_group_or_atom();
                if inner.chars().count() <= 1 {
                    format!("√{inner}")
                } else {
                    format!("√({inner})")
                }
            }
            "text" | "textrm" | "mathrm" | "mathbf" | "mathit" | "mathsf" | "texttt" | "operatorname" => {
                self.parse_group_or_atom()
            }
            "mathbb" => match self.parse_group_or_atom().as_str() {
                "R" => "ℝ".into(),
                "N" => "ℕ".into(),
                "Z" => "ℤ".into(),
                "Q" => "ℚ".into(),
                "C" => "ℂ".into(),
                "E" => "𝔼".into(),
                "P" => "ℙ".into(),
                other => other.to_owned(),
            },
            "hat" => format!("{}̂", self.parse_group_or_atom()),
            "bar" | "overline" => format!("{}̄", self.parse_group_or_atom()),
            "vec" => format!("{}⃗", self.parse_group_or_atom()),
            "quad" | "qquad" => "  ".into(),
            _ => tex_symbol(cmd).map(str::to_owned).unwrap_or_else(|| format!("\\{cmd}")),
        }
    }
}

fn tex_symbol(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ", "epsilon" | "varepsilon" => "ε",
        "zeta" => "ζ", "eta" => "η", "theta" | "vartheta" => "θ", "iota" => "ι", "kappa" => "κ",
        "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ", "pi" | "varpi" => "π",
        "rho" | "varrho" => "ρ", "sigma" | "varsigma" => "σ", "tau" => "τ", "upsilon" => "υ",
        "phi" | "varphi" => "φ", "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ", "Xi" => "Ξ",
        "Pi" => "Π", "Sigma" => "Σ", "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        "sum" => "∑", "prod" => "∏", "int" => "∫", "oint" => "∮", "infty" => "∞",
        "pm" => "±", "mp" => "∓", "times" => "×", "div" => "÷", "cdot" | "bullet" => "·",
        "le" | "leq" => "≤", "ge" | "geq" => "≥", "ne" | "neq" => "≠", "approx" => "≈",
        "equiv" => "≡", "sim" => "∼", "simeq" => "≃", "cong" => "≅", "propto" => "∝",
        "to" | "rightarrow" => "→", "leftarrow" | "gets" => "←", "leftrightarrow" => "↔",
        "Rightarrow" | "implies" => "⇒", "Leftarrow" => "⇐", "Leftrightarrow" | "iff" => "⇔",
        "mapsto" => "↦", "in" => "∈", "notin" => "∉", "ni" => "∋",
        "subset" => "⊂", "subseteq" => "⊆", "supset" => "⊃", "supseteq" => "⊇",
        "cup" => "∪", "cap" => "∩", "emptyset" | "varnothing" => "∅",
        "forall" => "∀", "exists" => "∃", "nexists" => "∄",
        "nabla" => "∇", "partial" => "∂", "land" | "wedge" => "∧", "lor" | "vee" => "∨",
        "lnot" | "neg" => "¬", "top" => "⊤", "bot" => "⊥",
        "dots" | "ldots" => "…", "cdots" => "⋯", "vdots" => "⋮", "ddots" => "⋱",
        "angle" => "∠", "perp" => "⊥", "parallel" => "∥", "circ" => "∘", "star" => "⋆",
        "lfloor" => "⌊", "rfloor" => "⌋", "lceil" => "⌈", "rceil" => "⌉",
        "langle" => "⟨", "rangle" => "⟩", "mid" => "∣",
        "sin" => "sin", "cos" => "cos", "tan" => "tan", "log" => "log", "ln" => "ln",
        "exp" => "exp", "lim" => "lim", "max" => "max", "min" => "min", "sup" => "sup", "inf" => "inf",
        "argmax" => "argmax", "argmin" => "argmin", "det" => "det", "dim" => "dim", "mod" => "mod",
        _ => return None,
    })
}

fn format_superscript(input: &str) -> String {
    let mut converted = String::with_capacity(input.len());
    for ch in input.chars() {
        let mapped = match ch {
            '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴',
            '5' => '⁵', '6' => '⁶', '7' => '⁷', '8' => '⁸', '9' => '⁹',
            '+' => '⁺', '-' | '−' => '⁻', '=' => '⁼', '(' => '⁽', ')' => '⁾',
            'n' => 'ⁿ', 'i' => 'ⁱ', 'T' => 'ᵀ',
            _ => return if input.chars().count() == 1 { format!("^{input}") } else { format!("^({input})") },
        };
        converted.push(mapped);
    }
    converted
}

fn format_subscript(input: &str) -> String {
    let mut converted = String::with_capacity(input.len());
    for ch in input.chars() {
        let mapped = match ch {
            '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄',
            '5' => '₅', '6' => '₆', '7' => '₇', '8' => '₈', '9' => '₉',
            '+' => '₊', '-' | '−' => '₋', '=' => '₌', '(' => '₍', ')' => '₎',
            'a' => 'ₐ', 'e' => 'ₑ', 'h' => 'ₕ', 'i' => 'ᵢ', 'j' => 'ⱼ',
            'k' => 'ₖ', 'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ',
            'p' => 'ₚ', 'r' => 'ᵣ', 's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ', 'v' => 'ᵥ', 'x' => 'ₓ',
            _ => return if input.chars().count() == 1 { format!("_{input}") } else { format!("_({input})") },
        };
        converted.push(mapped);
    }
    converted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_tex_symbols_scripts_and_fractions_cleanly() {
        assert_eq!(render_tex_unicode(r"\alpha^2 + \beta_i = \frac{1}{2}"), "α² + βᵢ = 1/2");
        assert_eq!(render_tex_unicode(r"\sum_{i=1}^n x_i \to \infty"), "∑ᵢ₌₁ⁿ xᵢ → ∞");
        assert_eq!(render_tex_unicode(r"x \in \mathbb{R}, \sqrt{a+b}"), "x ∈ ℝ, √(a+b)");
    }

    #[test]
    fn skips_currency_and_code_while_extracting_closed_block_and_inline_math() {
        let src = "Price is $10 and $20.\n\nInline $x^2 + 1$ and `$a+b$`.\n\n$$\nE = mc^2\n$$\n\n$$\nunclosed";
        let out = preprocess_math_for_display(src);
        assert!(out.contains("Price is $10 and $20."));
        assert!(out.contains("Inline x² + 1 and `$a+b$`."));
        assert!(out.contains("```t3-math\nE = mc^2\n```"));
        assert!(out.ends_with("$$\nunclosed"));
    }
}

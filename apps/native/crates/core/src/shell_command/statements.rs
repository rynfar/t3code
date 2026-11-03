//! Statement boundaries from `work-log/commandLabel.ts:splitFirstShellCommand`.

pub(super) fn statements(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut statement = String::new();
    let mut quote = None;
    let mut here_string = None;
    let mut escaped = false;
    let mut parentheses = 0;
    let mut expansion = 0;
    let mut heredocs = Vec::new();
    let mut index = 0;
    while index < bytes.len() && found.len() < 64 {
        let ch = source[index..].chars().next().unwrap();
        let next = bytes.get(index + 1).copied();
        if let Some(delimiter) = here_string {
            if bytes[index] == delimiter && next == Some(b'@') && (index == 0 || bytes[index - 1] == b'\n') {
                here_string = None;
            }
        } else if escaped {
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(delimiter) = quote {
            if ch == delimiter { quote = None; }
        } else if ch == '@' && matches!(next, Some(b'\'' | b'"')) && matches!(bytes.get(index + 2), Some(b'\n' | b'\r')) {
            here_string = next;
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if ch == '#' && (index == 0 || source[..index].ends_with(|c: char| c.is_whitespace() || ";&|(".contains(c))) {
            index = source[index..].find('\n').map_or(source.len(), |offset| index + offset);
            continue;
        } else if ch == '$' && next == Some(b'{') {
            expansion += 1;
            statement.push_str("${");
            index += 2;
            continue;
        } else if ch == '{' && expansion > 0 {
            expansion += 1;
        } else if ch == '}' && expansion > 0 {
            expansion -= 1;
        } else if ch == '(' {
            parentheses += 1;
        } else if ch == ')' && parentheses > 0 {
            parentheses -= 1;
        } else if parentheses == 0 && expansion == 0 {
            if ch == '<' && next == Some(b'<') && bytes.get(index + 2) != Some(&b'<') {
                let strip_tabs = bytes.get(index + 2) == Some(&b'-');
                let start = index + if strip_tabs { 3 } else { 2 };
                let Some((delimiter, end)) = delimiter(source, start) else {
                    statement.push_str(&source[index..]);
                    break;
                };
                heredocs.push((delimiter, strip_tabs));
                statement.push_str(&source[index..end]);
                index = end;
                continue;
            }
            let redirect = ch == '&' && (index > 0 && matches!(bytes[index - 1], b'>' | b'<') || next == Some(b'>'));
            if ";&|\n".contains(ch) && !redirect {
                found.push(std::mem::take(&mut statement));
                index += if (ch == '&' && next == Some(b'&')) || (ch == '|' && matches!(next, Some(b'|' | b'&'))) { 2 } else { 1 };
                if ch == '\n' {
                    for (delimiter, strip_tabs) in heredocs.drain(..) {
                        while index < source.len() {
                            let end = source[index..].find('\n').map_or(source.len(), |offset| index + offset);
                            let line = source[index..end].trim_end_matches('\r');
                            let line = if strip_tabs { line.trim_start_matches('\t') } else { line };
                            index = (end + 1).min(source.len());
                            if line == delimiter { break; }
                        }
                    }
                }
                continue;
            }
        }
        statement.push(ch);
        index += ch.len_utf8();
    }
    if found.len() < 64 { found.push(statement); }
    found
}

fn delimiter(source: &str, mut index: usize) -> Option<(String, usize)> {
    while index < source.len() && source[index..].starts_with(char::is_whitespace) { index += source[index..].chars().next()?.len_utf8(); }
    let mut delimiter = String::new();
    let mut quote = None;
    let mut escaped = false;
    while index < source.len() {
        let ch = source[index..].chars().next()?;
        if escaped {
            delimiter.push(ch);
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(end) = quote {
            if ch == end { quote = None; } else { delimiter.push(ch); }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch.is_whitespace() || ";&|<>()".contains(ch) {
            break;
        } else {
            delimiter.push(ch);
        }
        index += ch.len_utf8();
    }
    (!delimiter.is_empty() && quote.is_none() && !escaped).then_some((delimiter, index))
}

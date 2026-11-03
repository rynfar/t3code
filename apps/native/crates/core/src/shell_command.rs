//! Embedded script byte mappings from `apps/web/src/lib/embeddedScripts.ts`.

pub mod grammars;
mod statements;

use std::ops::Range;

pub struct EmbeddedScript {
    pub language: &'static str,
    pub text: String,
    pub spans: Vec<Range<usize>>,
}

/// `commandHighlightLanguage` from `work-log/commandLabel.ts`.
pub fn highlight_language(command: &str) -> &'static str {
    let command = command.trim();
    if command.strip_prefix('&').is_some_and(|tail| !tail.trim().is_empty()) {
        return "powershell";
    }
    let program = if let Some(quote @ ('\"' | '\'')) = command.chars().next() {
        command[1..].split(quote).next().unwrap_or_default()
    } else {
        command.split_whitespace().next().unwrap_or_default()
    };
    if matches!(basename(program).as_str(), "pwsh" | "powershell") {
        return "powershell";
    }
    let verbs = "Add Approve Assert Backup Block Build Checkpoint Clear Close Compare Complete Compress Confirm Connect Convert ConvertFrom ConvertTo Copy Debug Deny Deploy Disable Disconnect Dismount Edit Enable Enter Exit Expand Export Find ForEach Format Get Grant Group Hide Import Initialize Install Invoke Join Limit Lock Measure Merge Mount Move New Open Optimize Out Ping Pop Protect Publish Push Read Receive Redo Register Remove Rename Repair Request Reset Resize Resolve Restart Restore Resume Revoke Save Search Select Send Set Show Skip Sort Split Start Step Stop Submit Suspend Switch Sync Tee Test Trace Unblock Undo Uninstall Unlock Unprotect Unpublish Unregister Update Use Wait Watch Where Write";
    let is_powershell = |statement: &str| {
        let statement = statement.trim();
        if let Some(variable) = statement.strip_prefix('$') {
            let lower = variable.to_lowercase();
            if ["env:", "global:", "local:", "script:"].iter().any(|prefix| lower.starts_with(prefix)) {
                return true;
            }
            if let Some((name, rest)) = variable.split_once(char::is_whitespace)
                && name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
                && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                && rest.trim_start().starts_with('=')
            {
                return true;
            }
        }
        let word = statement.split_whitespace().next().unwrap_or_default();
        word.split_once('-').is_some_and(|(verb, noun)| {
            verbs.split_whitespace().any(|known| known == verb) && noun.starts_with(|ch: char| ch.is_ascii_uppercase()) && noun.chars().all(|ch| ch.is_ascii_alphabetic())
        })
    };
    if statements::statements(command).iter().any(|statement| is_powershell(statement)) { "powershell" } else { "bash" }
}

pub fn embedded_scripts(source: &str, tree: &tree_sitter::Tree) -> Vec<EmbeddedScript> {
    if tree.root_node().has_error() {
        return Vec::new();
    }
    let mut found = Vec::new();
    visit(tree.root_node(), source, &mut found);
    found.retain(|script| !script.text.trim().is_empty());
    found
}

fn visit(node: tree_sitter::Node<'_>, source: &str, found: &mut Vec<EmbeddedScript>) {
    if node.kind() == "command" {
        command_scripts(node, source, found);
    }
    for child in node.named_children(&mut node.walk()) {
        visit(child, source, found);
    }
}

fn expansion_ranges(node: tree_sitter::Node<'_>, ranges: &mut Vec<Range<usize>>) {
    if matches!(node.kind(), "expansion" | "simple_expansion" | "command_substitution" | "arithmetic_expansion" | "process_substitution") {
        ranges.push(node.byte_range());
        return;
    }
    for child in node.named_children(&mut node.walk()) {
        expansion_ranges(child, ranges);
    }
}

fn mapped_word(node: tree_sitter::Node<'_>, source: &str) -> Option<EmbeddedScript> {
    let spelling = &source[node.byte_range()];
    let mut expansions = Vec::new();
    expansion_ranges(node, &mut expansions);
    let mut text = String::new();
    let mut spans = Vec::new();
    let bytes = spelling.as_bytes();
    let mut quote = 0;
    let mut index = 0;
    while index < bytes.len() {
        if let Some(range) = expansions.iter().find(|range| range.start == node.start_byte() + index) {
            text.push_str(&source[range.clone()]);
            spans.extend(range.clone().map(|offset| offset..offset + 1));
            index = range.end - node.start_byte();
            continue;
        }
        let byte = bytes[index];
        if quote == 0 && byte == b'$' && matches!(bytes.get(index + 1), Some(b'\'' | b'"')) {
            if bytes[index + 1] == b'\'' && spelling[index..].contains('\\') {
                return None;
            }
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') && (quote == 0 || quote == byte) {
            quote = if quote == 0 { byte } else { 0 };
            index += 1;
            continue;
        }
        if byte == b'\\'
            && quote != b'\''
            && let Some(next) = bytes.get(index + 1)
            && (quote == 0 || b"$`\"\\\n".contains(next))
        {
            if *next != b'\n' {
                let ch = spelling[index + 1..].chars().next()?;
                text.push(ch);
                spans.extend(std::iter::repeat_n(node.start_byte() + index..node.start_byte() + index + 1 + ch.len_utf8(), ch.len_utf8()));
                index += ch.len_utf8() + 1;
            } else {
                index += 2;
            }
            continue;
        }
        let ch = spelling[index..].chars().next()?;
        text.push(ch);
        spans.extend((0..ch.len_utf8()).map(|offset| node.start_byte() + index + offset..node.start_byte() + index + offset + 1));
        index += ch.len_utf8();
    }
    Some(EmbeddedScript { language: "bash", text, spans })
}

fn basename(word: &str) -> String {
    word.rsplit(['/', '\\']).next().unwrap_or(word).to_lowercase().trim_end_matches(".exe").to_owned()
}

fn command_scripts(node: tree_sitter::Node<'_>, source: &str, found: &mut Vec<EmbeddedScript>) {
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    let mut words = vec![name];
    words.extend(node.children_by_field_name("argument", &mut node.walk()));
    let values: Vec<_> = words.iter().map(|node| mapped_word(*node, source)).collect();
    let value = |index: usize| values.get(index).and_then(Option::as_ref).map(|word| word.text.as_str()).unwrap_or("");
    let program_name = |index: usize| {
        let raw = words.get(index).map(|word| &source[word.byte_range()]).unwrap_or_default();
        // Windows paths are literal program names even when Bash reads their slashes as escapes.
        basename(if raw.contains('\\') { raw.trim_matches(['\'', '\"']) } else { value(index) })
    };
    let mut index = 0;
    while index < words.len() {
        let program = program_name(index);
        let options: &[&str] = match program.as_str() {
            "env" => &["-u", "--unset", "-C", "--chdir"],
            "sudo" => &["-u", "--user", "-g", "--group", "-C", "-D", "--chdir"],
            "nice" => &["-n", "--adjustment"],
            "exec" => &["-a"],
            "nohup" | "command" | "builtin" => &[],
            _ => break,
        };
        index += 1;
        let mut ended = false;
        while index < words.len() {
            let arg = value(index);
            if !ended && arg.starts_with('-') {
                ended = arg == "--";
                index += if options.contains(&arg) { 2 } else { 1 };
            } else if arg.split_once('=').is_some_and(|(key, _)| !key.is_empty() && key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')) {
                index += 1;
            } else {
                break;
            }
        }
    }
    let program = program_name(index);
    let language = match program.as_str() {
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" => Some("bash"),
        "fish" => Some("fish"),
        "node" => Some("javascript"),
        "bun" | "deno" => Some("typescript"),
        "ruby" => Some("ruby"),
        "perl" => Some("perl"),
        "osascript" => Some("applescript"),
        "pwsh" | "powershell" => Some("powershell"),
        "psql" | "mysql" | "mariadb" | "sqlite3" | "duckdb" => Some("sql"),
        name if name == "py" || name == "pypy" || name == "pypy3" || name.strip_prefix("python").is_some_and(|tail| tail.chars().all(|ch| ch.is_ascii_digit() || ch == '.')) => {
            Some("python")
        }
        _ => None,
    };
    let sql = language == Some("sql");
    let repeatable = sql;
    let mut from_file = false;
    let mut inlines = 0;
    let mut positionals = 0;
    let mut ended = false;
    let mut arg = index + 1;
    while let Some(language) = language
        && arg < words.len()
    {
        let original = value(arg);
        let lower = original.to_lowercase();
        let flag = if language == "powershell" { lower.as_str() } else { original };
        if !ended && flag == "--" {
            ended = true;
            arg += 1;
            continue;
        }
        let (name, attached) = if flag.starts_with("--") { flag.split_once('=').map_or((flag, false), |(name, _)| (name, true)) } else { (flag, false) };
        if !ended && flag.starts_with('-') && flag != "-" {
            let file = match language {
                "python" => name == "-m",
                "powershell" => ["-f", "-file", "-encodedcommand"].contains(&name),
                "sql" => ["-f", "--file"].contains(&name) && ["psql", "duckdb"].contains(&program.as_str()),
                _ => false,
            };
            if file {
                from_file = true;
                if !repeatable {
                    break;
                }
                arg += if attached { 1 } else { 2 };
                continue;
            }
            let inline = match language {
                "bash" => name.starts_with('-') && !name.starts_with("--") && name[1..].chars().all(|ch| ch.is_ascii_lowercase()) && name.contains('c'),
                "python" => name == "-c",
                "fish" => ["-c", "--command"].contains(&name),
                "javascript" | "typescript" if program != "deno" => ["-e", "--eval", "-p", "--print"].contains(&name),
                "ruby" | "applescript" => name == "-e",
                "perl" => ["-e", "-E"].contains(&name),
                "powershell" => ["-c", "-command"].contains(&name),
                "sql" => match program.as_str() {
                    "psql" => ["-c", "--command"].contains(&name),
                    "mysql" | "mariadb" => ["-e", "--execute"].contains(&name),
                    "sqlite3" => name == "-cmd",
                    _ => ["-c", "-cmd"].contains(&name),
                },
                _ => false,
            };
            if inline {
                let script_index = if attached { arg } else { arg + 1 };
                if let Some(Some(word)) = values.get(script_index) {
                    let start = if attached { word.text.find('=').map_or(0, |offset| offset + 1) } else { 0 };
                    found.push(EmbeddedScript { language, text: word.text[start..].to_owned(), spans: word.spans[start..].to_vec() });
                    inlines += 1;
                }
                if !repeatable {
                    break;
                }
                arg += if attached { 1 } else { 2 };
                continue;
            }
            let takes_value = match language {
                "bash" => ["-o", "-O", "--rcfile", "--init-file"].contains(&name),
                "python" => ["-W", "-X"].contains(&name),
                "javascript" => ["-r", "--require", "--import", "--loader", "-C", "--conditions"].contains(&name),
                "typescript" if program == "deno" => ["-c", "--config", "--import-map", "--ext"].contains(&name),
                "ruby" => ["-r", "-I", "-C"].contains(&name),
                "applescript" => ["-l", "-s"].contains(&name),
                "powershell" => ["-executionpolicy", "-workingdirectory", "-windowstyle"].contains(&name),
                "sql" => ["-d", "-h", "-u", "-U", "--dbname", "--host", "--user", "--username", "-separator", "-nullvalue", "-init"].contains(&name),
                _ => false,
            };
            arg += if takes_value && !attached { 2 } else { 1 };
            continue;
        }
        if program == "deno" {
            if positionals == 0 && flag != "eval" {
                from_file = true;
                break;
            }
            if positionals == 1 {
                if let Some(word) = &values[arg] {
                    found.push(EmbeddedScript { language, text: word.text.clone(), spans: word.spans.clone() });
                    inlines += 1;
                }
                break;
            }
        } else if ["sqlite3", "duckdb"].contains(&program.as_str()) && positionals >= 1 {
            if let Some(word) = &values[arg] {
                found.push(EmbeddedScript { language, text: word.text.clone(), spans: word.spans.clone() });
                inlines += 1;
            }
        } else if !sql {
            from_file = flag != "-";
            break;
        }
        positionals += 1;
        arg += 1;
    }
    let stdin = language.filter(|language| *language != "powershell" && !["bun", "deno"].contains(&program.as_str()) && inlines == 0 && !from_file);
    let mut redirects: Vec<_> = node.children_by_field_name("redirect", &mut node.walk()).collect();
    if let Some(parent) = node.parent().filter(|parent| parent.kind() == "redirected_statement") {
        redirects.extend(parent.children_by_field_name("redirect", &mut parent.walk()));
    }
    let written = if program == "cat" {
        redirects
            .iter()
            .filter(|r| r.kind() == "file_redirect" && descriptor(**r, source, "1"))
            .filter_map(|r| r.child_by_field_name("destination"))
            .next_back()
            .map(|n| source[n.byte_range()].to_owned())
    } else if program == "tee" {
        (index + 1..words.len()).find(|ix| !value(*ix).starts_with('-')).map(|ix| value(ix).to_owned())
    } else {
        None
    };
    let written_language = written.as_deref().and_then(file_language);
    for redirect in redirects {
        if redirect.kind() == "herestring_redirect"
            && descriptor(redirect, source, "0")
            && let Some(language) = stdin
            && let Some(target) = redirect.named_children(&mut redirect.walk()).find(|n| n.kind() != "file_descriptor")
            && let Some(mut script) = mapped_word(target, source)
        {
            script.language = language;
            found.push(script);
        }
        if redirect.kind() != "heredoc_redirect" {
            continue;
        }
        let children: Vec<_> = redirect.named_children(&mut redirect.walk()).collect();
        let delimiter = children.iter().find(|n| n.kind() == "heredoc_start").map(|n| source[n.byte_range()].trim_matches(['\'', '"']).to_lowercase());
        let language = if descriptor(redirect, source, "0") { stdin.or(written_language) } else { None }.or_else(|| delimiter.as_deref().and_then(delimiter_language));
        if let Some(language) = language
            && let Some(body) = children.iter().find(|n| n.kind() == "heredoc_body")
        {
            let range = body.byte_range();
            found.push(EmbeddedScript { language, text: source[range.clone()].to_owned(), spans: range.map(|offset| offset..offset + 1).collect() });
        }
    }
}

fn descriptor(node: tree_sitter::Node<'_>, source: &str, fd: &str) -> bool {
    node.child_by_field_name("descriptor").is_none_or(|n| &source[n.byte_range()] == fd)
}

fn delimiter_language(name: &str) -> Option<&'static str> {
    Some(match name {
        "py" | "python" => "python",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "sh" | "bash" | "shell" => "bash",
        "yml" | "yaml" => "yaml",
        "rb" | "ruby" => "ruby",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "json" => "json",
        "toml" => "toml",
        "html" => "html",
        "css" => "css",
        "xml" => "xml",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

fn file_language(path: &str) -> Option<&'static str> {
    let path = path.trim_matches(['\'', '"']);
    if matches!(path.rsplit(['/', '\\']).next()?, "Gemfile" | "Rakefile" | "Guardfile" | "Capfile" | "Berksfile" | "Brewfile" | "Vagrantfile" | "Thorfile" | "Appraisals" | "Dangerfile") {
        return Some("ruby");
    }
    crate::files::syntax_language(path).or_else(|| {
        let extension = path.rsplit('.').next()?.to_ascii_lowercase();
        match extension.as_str() {
            "ps1" | "psm1" | "psd1" => Some("powershell"),
            "pl" => Some("prolog"),
            "pm" => Some("perl"),
            "rake" | "gemspec" | "jbuilder" | "builder" | "rabl" | "arb" | "ru" | "podspec" => Some("ruby"),
            "fish" => Some("fish"),
            "applescript" | "scpt" => Some("applescript"),
            "xsl" | "xslt" => Some("xml"),
            _ => delimiter_language(&extension),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripts(source: &str) -> Vec<EmbeddedScript> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_bash::LANGUAGE.into()).unwrap();
        embedded_scripts(source, &parser.parse(source, None).unwrap())
    }

    #[test]
    fn maps_inline_scripts_and_heredocs_to_original_utf8_bytes() {
        for (command, language) in [
            ("pwsh -Command 'Get-Item .'", "powershell"),
            ("Get-ChildItem .", "powershell"),
            ("$env:PATH = 'x'", "powershell"),
            ("&'C:\\Tools\\pwsh.exe' -c Get-Item", "powershell"),
            ("cd src; Get-Item .", "powershell"),
            ("echo 'Get-Item .; hi'", "bash"),
            ("node -e 'console.log(1)'", "bash"),
            ("perl <<'EOF'\n$n = 2; print $n;\nEOF", "bash"),
            ("cat > app.ps1 <<'EOF'\nGet-Item .\nEOF", "bash"),
            ("echo ok # ignored; Get-Item .\necho done", "bash"),
            ("echo $(printf 'x'; Get-Item .)", "bash"),
            ("cat <<-'EOF' <<NEXT\n\tGet-Item .\n\tEOF\n$n = 2\nNEXT\nGet-Item .", "powershell"),
            ("cat <<EOF; Get-Item .\n$n = 2\nEOF", "powershell"),
        ] {
            assert_eq!(highlight_language(command), language, "{command}");
        }
        for (source, language, expected) in [
            ("python3 -c 'print(\"hé\")'", "python", "print(\"hé\")"),
            ("fish --command='set n 2; echo $n'", "fish", "set n 2; echo $n"),
            ("ruby -r json -e 'puts JSON.generate(2)'", "ruby", "puts JSON.generate(2)"),
            ("perl -E 'my $n = 2; say $n'", "perl", "my $n = 2; say $n"),
            ("osascript -l AppleScript -e 'set n to 2'", "applescript", "set n to 2"),
            ("osascript <<'EOF'\nset n to 2\nEOF", "applescript", "set n to 2\n"),
            ("env pwsh -COMMAND 'Get-Item .'", "powershell", "Get-Item ."),
            ("cat > config.xml <<'EOF'\n<item n=\"2\"/>\nEOF", "xml", "<item n=\"2\"/>\n"),
            (r#"C:\Python312\python.exe -c 'print(1)'"#, "python", "print(1)"),
            (r#"node -e "console.log($(echo 'x'))""#, "javascript", "console.log($(echo 'x'))"),
            ("node --eval=\"process.exit(0)\"", "javascript", "process.exit(0)"),
            ("sqlite3 app.db 'select 1'", "sql", "select 1"),
            ("deno eval --config deno.json 'console.log(1)'", "typescript", "console.log(1)"),
            ("psql -f schema.sql -c 'select 1'", "sql", "select 1"),
            ("python3 <<< 'print(1)'", "python", "print(1)"),
            ("env FOO=1 sudo -u bob node --eval=\"console.log(1)\"", "javascript", "console.log(1)"),
            ("python3 - <<'PY'\nprint(1)\nPY", "python", "print(1)\n"),
            ("cat > src/app.ts <<'EOF'\nconst a = 1;\nEOF", "typescript", "const a = 1;\n"),
            ("cat > main.rs <<'EOF'\nfn main() {}\nEOF", "rust", "fn main() {}\n"),
            ("cat > main.go <<'EOF'\npackage main\nEOF", "go", "package main\n"),
            ("tee app.tsx <<'EOF'\nconst app = <div />;\nEOF", "tsx", "const app = <div />;\n"),
            ("cat > readme.md <<'EOF'\n# Title\nEOF", "markdown", "# Title\n"),
            ("cat > app.ps1 <<'EOF'\nGet-Item .\nEOF", "powershell", "Get-Item .\n"),
            ("tee app.fish <<'EOF'\nset n 2\nEOF", "fish", "set n 2\n"),
            ("cat > app.pl <<'EOF'\nmy $n = 2;\nEOF", "prolog", "my $n = 2;\n"),
            ("cat > app.applescript <<'EOF'\nset n to 2\nEOF", "applescript", "set n to 2\n"),
            ("kubectl apply -f - <<'YAML'\nkind: Pod\nYAML", "yaml", "kind: Pod\n"),
            ("echo \"$(node -e 'console.log(1)')\"", "javascript", "console.log(1)"),
        ] {
            let found = scripts(source);
            assert_eq!(found.len(), 1, "{source}");
            assert_eq!((found[0].language, found[0].text.as_str()), (language, expected), "{source}");
            assert_eq!(found[0].spans.len(), expected.len());
            for span in &found[0].spans {
                assert!(span.start < span.end && span.end <= source.len());
            }
        }
    }

    #[test]
    fn preserves_escaped_nested_shell_mappings_and_skips_non_scripts() {
        let source = r#"/bin/bash -lc "node -e \"console.log('hi')\"""#;
        let found = scripts(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "node -e \"console.log('hi')\"");
        let quote = found[0].text.find('"').unwrap();
        assert_eq!(&source[found[0].spans[quote].clone()], "\\\"");
        for source in ["python3 build.py <<EOF\ndata\nEOF", "python3 -m json.tool <<EOF\n{}\nEOF", "rg -n 'foo' src", "bash -lc ' '", "node -e 'broken"] {
            assert!(scripts(source).is_empty(), "{source}");
        }
    }
}

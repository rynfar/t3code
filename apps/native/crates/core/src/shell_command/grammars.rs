//! Extra grammars for the languages requested by web `ShellCommandBlock` / `embeddedScripts.ts`.

pub struct Grammar {
    pub language: tree_sitter::Language,
    pub highlights: String,
}

pub const LANGUAGES: [&str; 3] = ["powershell", "perl", "xml"];

pub fn grammar(name: &str) -> Option<Grammar> {
    let (language, highlights) = match name {
        "powershell" => (
            tree_sitter_powershell::LANGUAGE.into(),
            tree_sitter_powershell::HIGHLIGHTS_QUERY
                .replace("(array_expression) @array", "")
                .replace("(assignment_expression\n  value: (pipeline) @assignvalue)", "")
                .replace("@delimiter", "@punctuation.delimiter")
                .replace("(variable) @variable", r#"
((variable) @variable (#not-match? @variable "(?i)^\\$(true|false|null)$"))
((variable) @boolean (#match? @boolean "(?i)^\\$(true|false)$"))
((variable) @constant (#match? @constant "(?i)^\\$null$"))
"#)
                + r#"
(command_parameter) @variable.parameter
["(" ")" "{" "}" "[" "]"] @punctuation.bracket
["=" "|"] @operator
"#,
        ),
        "perl" => {
            let mut query = ts_parser_perl::HIGHLIGHTS_QUERY.to_owned();
            for capture in ["include", "conditional", "repeat", "exception"] {
                query = query.replace(&format!("@{capture}"), "@keyword");
            }
            query = query.replace("@method", "@function").replace("@character.special", "@string.special");
            (ts_parser_perl::LANGUAGE.into(), query)
        }
        "xml" => (
            tree_sitter_xml::LANGUAGE_XML.into(),
            tree_sitter_xml::XML_HIGHLIGHT_QUERY
                .replace("(Attribute (Name) @property)", "(Attribute (Name) @attribute)")
                .replace("@constant.builtin", "@string.escape")
                .replace("(EntityRef) @constant", "(EntityRef) @string.escape")
                .replace("(CharRef) @constant", "(CharRef) @string.escape")
                .replace(r#"[ "\"" "'" ] @punctuation.delimiter"#, r#"[ "\"" "'" ] @string"#)
                .replace(r#"[ "," "|" "=" ] @operator"#, r#"[ "," "|" ] @operator
"=" @punctuation.delimiter"#),
        ),
        _ => return None,
    };
    Some(Grammar { language, highlights })
}

pub fn ruby_highlights(query: &str) -> String {
    query
        .replace("(escape_sequence) @escape", "(escape_sequence) @string.escape")
        .replace("@property", "@variable.special")
        .replace("  (true)\n  (false)\n", "")
        + r#"
[(true) (false)] @boolean
(binary operator: _ @operator)
(unary operator: _ @operator)
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::StreamingIterator;

    #[test]
    fn command_tokens_use_the_web_syntax_roles() {
        for (name, code, tokens) in [
            ("powershell", "$n = 2; Get-Item \"src\" # note", vec![("$n", "variable"), ("2", "number"), ("Get-Item", "function"), ("\"src\"", "string"), ("# note", "comment")]),
            ("powershell", "Get-Item -Path $env:HOME; if ($true) { Write-Host \"hi $n\" }", vec![("-Path", "variable"), ("$env:HOME", "variable"), ("$true", "boolean"), ("(", "punctuation"), ("$n", "variable")]),
            ("perl", "my $n = 2; print \"hi\"; # note", vec![("my", "keyword"), ("$n", "variable"), ("2", "number"), ("print", "function"), ("\"hi\"", "string"), ("# note", "comment")]),
            ("xml", "<item n=\"2\">hi</item><!-- note -->", vec![("item", "tag"), ("n", "attribute"), ("\"2\"", "string"), ("<!-- note -->", "comment")]),
            ("ruby", "if n == 2; puts \"hi\"; end # note\n", vec![("if", "keyword"), ("2", "number"), ("puts", "function"), ("\"hi\"", "string"), ("# note", "comment")]),
        ] {
            let grammar = if name == "ruby" {
                Grammar { language: tree_sitter_ruby::LANGUAGE.into(), highlights: ruby_highlights(tree_sitter_ruby::HIGHLIGHTS_QUERY) }
            } else {
                grammar(name).unwrap_or_else(|| panic!("missing {name} grammar"))
            };
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&grammar.language).unwrap();
            let tree = parser.parse(code, None).unwrap();
            assert!(!tree.root_node().has_error(), "{name}: {}", tree.root_node().to_sexp());
            let query = tree_sitter::Query::new(&grammar.language, &grammar.highlights).unwrap();
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), code.as_bytes());
            let mut captures = Vec::new();
            while let Some(matched) = matches.next() {
                for capture in matched.captures {
                    captures.push((capture.node.byte_range(), query.capture_names()[capture.index as usize]));
                }
            }
            for (token, role) in tokens {
                let start = code.find(token).unwrap();
                assert!(captures.iter().any(|(range, name)| range.start <= start && range.end >= start + token.len() && name.split('.').next() == Some(role)), "{name}: {token} needs {role}: {captures:?}");
            }
        }
    }
}

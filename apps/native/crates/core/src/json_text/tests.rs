use super::*;

fn value(text: &str) -> Value {
    serde_json::from_str(text).expect("valid JSON")
}

/// Expected strings are what Node prints for the same input.
#[test]
fn stringify_matches_javascript() {
    let parsed = value(
        r#"{"b":1,"10":2,"2":3,"a":[1,2,{}],"e":[],"s":"q\"\\\n\u0001\u007f","n":null,"f":1.5e-7,"g":1e21,"h":0.000001,"i":-0.5}"#,
    );
    assert_eq!(
        stringify_pretty(&parsed),
        "{\n  \"2\": 3,\n  \"10\": 2,\n  \"b\": 1,\n  \"a\": [\n    1,\n    2,\n    {}\n  ],\n  \"e\": [],\n  \"s\": \"q\\\"\\\\\\n\\u0001\u{7f}\",\n  \"n\": null,\n  \"f\": 1.5e-7,\n  \"g\": 1e+21,\n  \"h\": 0.000001,\n  \"i\": -0.5\n}"
    );
    assert_eq!(
        stringify(&parsed),
        "{\"2\":3,\"10\":2,\"b\":1,\"a\":[1,2,{}],\"e\":[],\"s\":\"q\\\"\\\\\\n\\u0001\u{7f}\",\"n\":null,\"f\":1.5e-7,\"g\":1e+21,\"h\":0.000001,\"i\":-0.5}"
    );
    assert_eq!(
        stringify(&value("[0.1, 100, 1e2, 2.5e+25, -1e-7, 1e300, 5e-324, 123.456, 9007199254740993, 1.0]")),
        "[0.1,100,100,2.5e+25,-1e-7,1e+300,5e-324,123.456,9007199254740992,1]"
    );
}

#[test]
fn only_text_json_parse_accepts_is_json() {
    assert!(is_json_text("{\"a\": [1, true, null]}"));
    assert!(is_json_text(" 42 "));
    assert!(is_json_text("\"text\""));
    assert!(!is_json_text(""));
    assert!(!is_json_text("{a: 1}"));
    assert!(!is_json_text("[1,]"));
    assert!(!is_json_text("plain words"));
}

#[test]
fn trim_takes_javascript_whitespace() {
    assert_eq!(js_trim("\u{feff} x \u{a0}\n"), "x");
    assert_eq!(js_trim("\u{85}x"), "\u{85}x", "NEL is not JavaScript whitespace");
}

fn runs(text: &str) -> Vec<(&str, &'static str)> {
    json_tokens(text).into_iter().map(|(range, token)| (&text[range], token.capture())).collect()
}

/// The runs `@pierre/highlights` gives the same JSON (whitespace aside).
#[test]
fn tokens_name_the_captures_the_web_highlighter_uses() {
    assert_eq!(
        runs(r#"{"a": "x\"y\n", "n": -1.5e3, "t": true, "z": null, "arr": [1, {"k": []}, "s"]}"#),
        vec![
            ("{", "punctuation.bracket"),
            ("\"a\"", "property.json_key"),
            (":", "punctuation.delimiter"),
            ("\"x", "string"),
            ("\\\"", "string.escape"),
            ("y", "string"),
            ("\\n", "string.escape"),
            ("\"", "string"),
            (",", "punctuation.delimiter"),
            ("\"n\"", "property.json_key"),
            (":", "punctuation.delimiter"),
            ("-1.5e3", "number"),
            (",", "punctuation.delimiter"),
            ("\"t\"", "property.json_key"),
            (":", "punctuation.delimiter"),
            ("true", "boolean"),
            (",", "punctuation.delimiter"),
            ("\"z\"", "property.json_key"),
            (":", "punctuation.delimiter"),
            ("null", "constant.builtin"),
            (",", "punctuation.delimiter"),
            ("\"arr\"", "property.json_key"),
            (":", "punctuation.delimiter"),
            ("[", "punctuation.bracket"),
            ("1", "number"),
            (",", "punctuation.delimiter"),
            ("{", "punctuation.bracket"),
            ("\"k\"", "property.json_key"),
            (":", "punctuation.delimiter"),
            ("[", "punctuation.bracket"),
            ("]", "punctuation.bracket"),
            ("}", "punctuation.bracket"),
            (",", "punctuation.delimiter"),
            ("\"s\"", "string"),
            ("]", "punctuation.bracket"),
            ("}", "punctuation.bracket"),
        ]
    );
    assert_eq!(runs(r#"{"a\"b": 1}"#)[1..4], [("\"a", "property.json_key"), ("\\\"", "string.escape"), ("b\"", "property.json_key")]);
    let unicode = format!("[\"{}00e9x\"]", "\\u");
    assert_eq!(runs(&unicode)[1..4], [("\"", "string"), ("\\u00e9", "string.escape"), ("x\"", "string")]);
    assert_eq!(runs("\"just a string\""), [("\"just a string\"", "string")]);
}

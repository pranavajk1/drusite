// SPDX-License-Identifier: AGPL-3.0-or-later

//! Code highlighting for published pages: a small lexer, no dependencies, no regular
//! expressions, linear in the length of the code. It knows comments, strings, numbers and the
//! keywords of the languages people write most; anything else is plain text. Every piece of the
//! code is escaped.

use super::render::escape_into;

/// How a language writes comments and strings, and its keywords.
struct Lang {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// Keywords match ignoring case (SQL).
    caseless: bool,
}

const C_LIKE_COMMENTS: (&[&str], Option<(&str, &str)>) = (&["//"], Some(("/*", "*/")));

fn lang(name: &str) -> Option<Lang> {
    let name = name.trim().to_ascii_lowercase();
    let (line_comments, block_comment) = C_LIKE_COMMENTS;
    let c_like = |keywords: &'static [&'static str], quotes: &'static [char]| Lang {
        line_comments,
        block_comment,
        quotes,
        keywords,
        caseless: false,
    };
    Some(match name.as_str() {
        "rust" | "rs" => c_like(RUST, &['"']),
        "javascript" | "js" | "jsx" | "typescript" | "ts" | "tsx" | "svelte" => {
            c_like(JS, &['"', '\'', '`'])
        }
        "c" | "cpp" | "c++" | "h" | "hpp" | "objc" | "objective-c" => c_like(C, &['"', '\'']),
        "java" | "kotlin" | "kt" | "scala" | "dart" | "csharp" | "c#" | "cs" | "swift" => {
            c_like(JAVA, &['"', '\''])
        }
        "go" | "golang" => c_like(GO, &['"', '\'', '`']),
        "css" | "scss" | "less" => Lang {
            line_comments: &[],
            block_comment: Some(("/*", "*/")),
            quotes: &['"', '\''],
            keywords: &["important", "inherit", "initial", "none", "auto"],
            caseless: false,
        },
        "python" | "py" => Lang {
            line_comments: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: PYTHON,
            caseless: false,
        },
        "ruby" | "rb" => Lang {
            line_comments: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: RUBY,
            caseless: false,
        },
        "shell" | "sh" | "bash" | "zsh" | "fish" | "console" => Lang {
            line_comments: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: SHELL,
            caseless: false,
        },
        "yaml" | "yml" | "toml" | "ini" | "dockerfile" | "makefile" | "nix" => Lang {
            line_comments: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: &["true", "false", "null", "yes", "no"],
            caseless: false,
        },
        "json" | "jsonc" => Lang {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: &['"'],
            keywords: &["true", "false", "null"],
            caseless: false,
        },
        "sql" | "postgres" | "postgresql" | "sqlite" | "mysql" => Lang {
            line_comments: &["--"],
            block_comment: Some(("/*", "*/")),
            quotes: &['\''],
            keywords: SQL,
            caseless: true,
        },
        "lua" => Lang {
            line_comments: &["--"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: LUA,
            caseless: false,
        },
        "haskell" | "hs" | "elm" => Lang {
            line_comments: &["--"],
            block_comment: Some(("{-", "-}")),
            quotes: &['"'],
            keywords: HASKELL,
            caseless: false,
        },
        "html" | "xml" | "svg" | "markdown" | "md" | "plain" | "text" | "" => return None,
        _ => return None,
    })
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const JS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];
const C: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "if",
    "include",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "nullptr",
    "private",
    "public",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "true",
    "typedef",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];
const JAVA: &[&str] = &[
    "abstract",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "data",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "fun",
    "func",
    "guard",
    "if",
    "implements",
    "import",
    "in",
    "int",
    "interface",
    "is",
    "let",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "string",
    "struct",
    "super",
    "switch",
    "this",
    "throw",
    "throws",
    "true",
    "try",
    "val",
    "var",
    "void",
    "when",
    "while",
];
const GO: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "false",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const PYTHON: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try",
    "while", "with", "yield",
];
const RUBY: &[&str] = &[
    "begin", "class", "def", "do", "else", "elsif", "end", "ensure", "false", "for", "if", "in",
    "module", "next", "nil", "not", "or", "raise", "rescue", "return", "self", "then", "true",
    "unless", "until", "when", "while", "yield",
];
const SHELL: &[&str] = &[
    "case", "do", "done", "echo", "elif", "else", "esac", "exit", "export", "fi", "for",
    "function", "if", "in", "local", "return", "then", "until", "while",
];
const SQL: &[&str] = &[
    "and",
    "as",
    "asc",
    "begin",
    "by",
    "case",
    "commit",
    "create",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "false",
    "from",
    "group",
    "having",
    "in",
    "index",
    "insert",
    "into",
    "is",
    "join",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "on",
    "or",
    "order",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "set",
    "table",
    "then",
    "true",
    "union",
    "update",
    "values",
    "when",
    "where",
    "with",
];
const LUA: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];
const HASKELL: &[&str] = &[
    "case", "class", "data", "deriving", "do", "else", "if", "import", "in", "instance", "let",
    "module", "newtype", "of", "then", "type", "where",
];

fn span(out: &mut String, class: &str, text: &str) {
    out.push_str("<span class=\"tok-");
    out.push_str(class);
    out.push_str("\">");
    escape_into(out, text);
    out.push_str("</span>");
}

/// The code as HTML: escaped, with `<span class="tok-…">` around the parts this lexer knows.
pub(super) fn highlight(language: &str, code: &str) -> String {
    let mut out = String::with_capacity(code.len() + code.len() / 4);
    let Some(lang) = lang(language) else {
        escape_into(&mut out, code);
        return out;
    };
    let mut rest = code;
    let mut plain = String::new();
    let flush = |out: &mut String, plain: &mut String| {
        escape_into(out, plain);
        plain.clear();
    };
    while let Some(c) = rest.chars().next() {
        if lang.line_comments.iter().any(|p| rest.starts_with(*p)) {
            flush(&mut out, &mut plain);
            let end = rest.find('\n').unwrap_or(rest.len());
            span(&mut out, "c", &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if let Some((open, close)) = lang.block_comment
            && rest.starts_with(open)
        {
            flush(&mut out, &mut plain);
            let end = rest[open.len()..]
                .find(close)
                .map_or(rest.len(), |i| open.len() + i + close.len());
            span(&mut out, "c", &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if lang.quotes.contains(&c) {
            flush(&mut out, &mut plain);
            let start = c.len_utf8();
            let mut end = rest.len();
            let mut escaped = false;
            for (i, ch) in rest[start..].char_indices() {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == c {
                    end = start + i + ch.len_utf8();
                    break;
                } else if ch == '\n' && c != '`' {
                    end = start + i;
                    break;
                }
            }
            span(&mut out, "s", &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if c.is_ascii_digit() {
            flush(&mut out, &mut plain);
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            span(&mut out, "n", &rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            let word = &rest[..end];
            let keyword = if lang.caseless {
                lang.keywords.iter().any(|k| k.eq_ignore_ascii_case(word))
            } else {
                lang.keywords.contains(&word)
            };
            if keyword {
                flush(&mut out, &mut plain);
                span(&mut out, "k", word);
            } else {
                plain.push_str(word);
            }
            rest = &rest[end..];
            continue;
        }
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut out, &mut plain);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_keywords_strings_comments_and_numbers() {
        let html = highlight("rust", "fn main() { let s = \"<b>\"; // done\n 42 }");
        assert!(html.contains(r#"<span class="tok-k">fn</span>"#), "{html}");
        assert!(
            html.contains(r#"<span class="tok-s">&quot;&lt;b&gt;&quot;</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="tok-c">// done</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="tok-n">42</span>"#), "{html}");
    }

    #[test]
    fn an_unknown_language_is_only_escaped() {
        assert_eq!(highlight("brainfun", "<script>&"), "&lt;script&gt;&amp;");
        assert_eq!(highlight("", "a < b"), "a &lt; b");
    }

    #[test]
    fn unterminated_pieces_run_to_the_end_safely() {
        for code in ["\"abc", "/* open", "'\\", "x = `\n<", "0x"] {
            for lang in ["rust", "js", "python", "sql", "haskell"] {
                let html = highlight(lang, code);
                assert!(
                    !html.contains('<') || html.contains("<span"),
                    "{lang} {code}: {html}"
                );
            }
        }
    }

    #[test]
    fn sql_keywords_ignore_case() {
        let html = highlight("sql", "SELECT 1 FROM t -- x");
        assert!(html.contains(r#"<span class="tok-k">SELECT</span>"#));
        assert!(html.contains(r#"<span class="tok-c">-- x</span>"#));
    }
}

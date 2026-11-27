// SPDX-License-Identifier: AGPL-3.0-or-later

//! Search results as people see them: snippets with the matching words marked. The device's
//! index and the server's search both make them this way.

use serde::{Deserialize, Serialize};
use specta::Type;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// How much text a snippet shows around the first match, in characters.
pub const SNIPPET_CHARS: usize = 160;

/// A piece of a search snippet; `highlight` marks the words that matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SnippetPart {
    pub text: String,
    pub highlight: bool,
}

/// The words of `text`, lower case with accents taken off, for matching.
pub fn fold_words(text: &str) -> Vec<String> {
    let folded: String = text
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A window of `text` around its first matching word, with every word that starts with one of
/// `words` (as [`fold_words`] gives them) marked.
pub fn snippet(text: &str, words: &[String]) -> Vec<SnippetPart> {
    // Find word boundaries the same way the index does, but keep the original text.
    let mut spans: Vec<(usize, usize, bool)> = Vec::new(); // byte ranges of words, matched?
    let mut start = None;
    for (i, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            let folded = fold_words(&text[s..i]).concat();
            let hit = words.iter().any(|w| folded.starts_with(w.as_str()));
            spans.push((s, i, hit));
        }
    }
    let first = spans.iter().find(|s| s.2).map_or(0, |s| s.0);
    // Start a little before the first hit, on a word boundary, and end after SNIPPET_CHARS.
    let lead = text[..first]
        .char_indices()
        .rev()
        .nth(40)
        .map_or(0, |(i, _)| i);
    let from = spans
        .iter()
        .map(|s| s.0)
        .find(|&s| s >= lead)
        .unwrap_or(first)
        .min(first);
    let to = text[from..]
        .char_indices()
        .nth(SNIPPET_CHARS)
        .map_or(text.len(), |(i, _)| from + i);

    let mut out: Vec<SnippetPart> = Vec::new();
    let mut push = |s: &str, highlight: bool| {
        if s.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.highlight == highlight => last.text.push_str(s),
            _ => out.push(SnippetPart {
                text: s.to_owned(),
                highlight,
            }),
        }
    };
    if from > 0 {
        push("…", false);
    }
    let mut at = from;
    for &(s, e, hit) in spans.iter().filter(|s| s.2 && s.0 >= from && s.1 <= to) {
        push(&text[at..s], false);
        push(&text[s..e], hit);
        at = e;
    }
    push(&text[at..to], false);
    if to < text.len() {
        push("…", false);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!(fold_words("Café NOIR, naïve!"), ["cafe", "noir", "naive"]);
    }

    #[test]
    fn marks_words_that_start_with_a_query_word() {
        let parts = snippet("Coffee at Café Noir", &fold_words("cafe no"));
        let shown: String = parts
            .iter()
            .map(|p| {
                if p.highlight {
                    format!("[{}]", p.text)
                } else {
                    p.text.clone()
                }
            })
            .collect();
        assert_eq!(shown, "Coffee at [Café] [Noir]");
    }

    #[test]
    fn snippets_window_long_text() {
        let long = format!("{} needle {}", "word ".repeat(60), "tail ".repeat(60));
        let parts = snippet(&long, &["needle".into()]);
        let text: String = parts.iter().map(|p| p.text.as_str()).collect();
        assert!(text.starts_with('…') && text.ends_with('…'));
        assert!(text.chars().count() < SNIPPET_CHARS + 50);
        assert!(parts.iter().any(|p| p.highlight && p.text == "needle"));
    }
}

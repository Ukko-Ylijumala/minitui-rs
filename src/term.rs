// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
What a filter's text asks, word by word: [`FilterTerm`] reads a word into
the scope it names, the text looked for and a comparison with what it is
against, as in `p:akismet<5.0`, `r:=wso-shell` or `s>=4`. Which scopes
there are, what a bound is a bound of (a version, a level) and what a
word matches stay with the app: a word that names no scope of its and
holds no bound it can read is still there as it was typed
([`word`](FilterTerm::word)), to be looked for as it stands.

[`Cmp`] is the comparison, and [`contains_folded`] the plain word's
search, in any case and without a copy of the text per row.

From the filters of wp-scanner and the malware scanner, which read the
same shape each with its own fields.
*/

use std::cmp::Ordering;

use strum::{EnumIter, IntoEnumIterator, IntoStaticStr};

/* ===== consts and statics ================================================= */

/// What separates a scope from what is looked for in it.
const SCOPE: char = ':';

/* ===== types ============================================================== */

/**
A comparison in a filter's bound, by its sign. The two-character signs
come first: `>=` is not `>` and then `=4`.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, IntoStaticStr)]
pub enum Cmp {
    #[strum(serialize = "<=")]
    Le,
    #[strum(serialize = ">=")]
    Ge,
    #[strum(serialize = "<")]
    Lt,
    #[strum(serialize = ">")]
    Gt,
    #[strum(serialize = "=")]
    Eq,
}

/**
One word of a filter's text, read: `scope:text<sign>bound`, each part
optional. Nothing is copied, and the case is the word's: an app that
filters in any case lowercases the text before it is read.

```
use minitui::{Cmp, FilterTerm};

let term = FilterTerm::read("p:akismet<5.0");
assert_eq!((term.scope, term.text, term.bound), (Some("p"), "akismet", Some((Cmp::Lt, "5.0"))));
// a scope the app does not have: the word again, the colon a part of its text
let term = FilterTerm::read("x:y").unscoped();
assert_eq!((term.scope, term.text), (None, "x:y"));
// every word of a filter, each of which must hold
let words: Vec<&str> = FilterTerm::words("r:wso  s>=4 uploads").map(|t| t.text).collect();
assert_eq!(words, ["wso", "s", "uploads"]);
```
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterTerm<'a> {
    /// The word as it was typed (trimmed), for one that is no scope and no bound after all.
    pub word: &'a str,
    /// Before the first `:`, when no comparison comes before it and it is not empty.
    pub scope: Option<&'a str>,
    /// What is looked for: what follows the scope, up to a comparison. Trimmed.
    pub text: &'a str,
    /// The first comparison after the scope and what follows it, trimmed.
    pub bound: Option<(Cmp, &'a str)>,
}

/* ===== Cmp ================================================================ */

impl Cmp {
    /// Its sign: `<=`.
    pub fn sign(self) -> &'static str {
        self.into()
    }

    /// Whether a value that is `ord` to the bound meets it.
    pub fn holds(self, ord: Ordering) -> bool {
        match self {
            Cmp::Le => ord != Ordering::Greater,
            Cmp::Ge => ord != Ordering::Less,
            Cmp::Lt => ord == Ordering::Less,
            Cmp::Gt => ord == Ordering::Greater,
            Cmp::Eq => ord == Ordering::Equal,
        }
    }

    /// Whether `value` meets `bound`; never when the two do not compare (a NaN).
    pub fn admits<T: PartialOrd + ?Sized>(self, value: &T, bound: &T) -> bool {
        value.partial_cmp(bound).is_some_and(|ord| self.holds(ord))
    }

    /// The comparison `text` begins with, and what follows its sign.
    pub fn strip(text: &str) -> Option<(Cmp, &str)> {
        Cmp::iter().find_map(|cmp| Some((cmp, text.strip_prefix(cmp.sign())?)))
    }

    /// Where the first sign in `text` is, with its comparison and what follows.
    fn split(text: &str) -> Option<(&str, Cmp, &str)> {
        let at = text.find(['<', '>', '='])?;
        let (before, tail) = text.split_at(at);
        Cmp::strip(tail).map(|(cmp, after)| (before, cmp, after))
    }
}

/* ===== FilterTerm ========================================================= */

impl<'a> FilterTerm<'a> {
    /**
    One word read. It need not be a word: a filter that is one term, white
    space and all (`woo <= 8`), is read the same, its text and its bound
    trimmed.
    */
    pub fn read(word: &'a str) -> Self {
        let word = word.trim();
        let scoped = word.split_once(SCOPE).filter(|(scope, _)| {
            !scope.is_empty() && !scope.contains(['<', '>', '=']) && !scope.contains(char::is_whitespace)
        });
        match scoped {
            Some((scope, rest)) => Self::of(word, Some(scope), rest),
            None => Self::of(word, None, word),
        }
    }

    /// The words of a filter's text, read: the terms a row must each meet.
    pub fn words(text: &'a str) -> impl Iterator<Item = FilterTerm<'a>> {
        text.split_whitespace().map(FilterTerm::read)
    }

    /**
    The word read as one with no scope, the colon a part of its text: for
    a scope that is none of the app's. `x:y<2` is then `x:y` below 2.
    */
    pub fn unscoped(&self) -> Self {
        Self::of(self.word, None, self.word)
    }

    /**
    Everything after the scope as it stands, trimmed: for a scope whose
    value is text whatever signs are in it, and for a bound that turned
    out not to be one (`a<b`, where `b` is no version).
    */
    pub fn value(&self) -> &'a str {
        match self.scope {
            Some(scope) => self.word[scope.len() + SCOPE.len_utf8()..].trim(),
            None => self.word,
        }
    }

    /**
    The value after an `=` that begins it: `r:=wso-shell` asks for the
    whole of the field, where `r:wso-shell` is a part of it.
    */
    pub fn whole(&self) -> Option<&'a str> {
        match self.bound {
            Some((Cmp::Eq, value)) if self.text.is_empty() => Some(value),
            _ => None,
        }
    }

    /**
    Whether the word is still being typed: a scope with nothing after it
    (`r:`), or a comparison with nothing after it (`s>=`, `r:=`). An app
    that lets such a word ask nothing keeps its list from emptying between
    two keys.
    */
    pub fn is_open(&self) -> bool {
        match self.bound {
            Some((_, value)) => value.is_empty(),
            None => self.scope.is_some() && self.text.is_empty(),
        }
    }

    fn of(word: &'a str, scope: Option<&'a str>, rest: &'a str) -> Self {
        let (text, bound) = match Cmp::split(rest) {
            Some((text, cmp, value)) => (text, Some((cmp, value.trim()))),
            None => (rest, None),
        };
        Self { word, scope, text: text.trim(), bound }
    }
}

/* ===== free functions ===================================================== */

/**
Whether `word` is in `text` in any case. `word` is lowercase already, as
a filter's text is once for all the rows it is asked of (past ASCII a
capital in it is found in no text); nothing is allocated for an ASCII
word, which a list filtered on every key of a hundred thousand rows is
glad of. The empty word is in every text.
*/
pub fn contains_folded(text: &str, word: &str) -> bool {
    if !word.is_ascii() {
        return text.to_lowercase().contains(word);
    }
    let (text, word) = (text.as_bytes(), word.as_bytes());
    word.is_empty() || (word.len() <= text.len() && text.windows(word.len()).any(|at| at.eq_ignore_ascii_case(word)))
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    /// A term's parts: the scope, the text, the bound.
    type Parts<'a> = (Option<&'a str>, &'a str, Option<(Cmp, &'a str)>);

    fn parts(word: &str) -> Parts<'_> {
        let term = FilterTerm::read(word);
        (term.scope, term.text, term.bound)
    }

    fn value(word: &str) -> &str {
        FilterTerm::read(word).value()
    }

    fn whole(word: &str) -> Option<&str> {
        FilterTerm::read(word).whole()
    }

    #[test]
    fn a_word_into_its_scope_its_text_and_its_bound() {
        assert_eq!(parts("akismet"), (None, "akismet", None));
        assert_eq!(parts("p:akismet"), (Some("p"), "akismet", None));
        assert_eq!(parts("p:akismet<5.0"), (Some("p"), "akismet", Some((Cmp::Lt, "5.0"))));
        assert_eq!(parts("c:>=6.4"), (Some("c"), "", Some((Cmp::Ge, "6.4"))));
        assert_eq!(parts("s>=4"), (None, "s", Some((Cmp::Ge, "4"))));
        assert_eq!(parts("r:=wso-shell"), (Some("r"), "", Some((Cmp::Eq, "wso-shell"))));
        // one term with white space in it: trimmed
        assert_eq!(parts(" woo <= 8 "), (None, "woo", Some((Cmp::Le, "8"))));
        assert_eq!(parts(""), (None, "", None));
    }

    #[test]
    fn every_sign_and_the_longer_one_first() {
        for (text, cmp) in [("<=1", Cmp::Le), (">=1", Cmp::Ge), ("<1", Cmp::Lt), (">1", Cmp::Gt), ("=1", Cmp::Eq)] {
            assert_eq!(Cmp::strip(text), Some((cmp, "1")), "{text}");
            assert_eq!(format!("{}1", cmp.sign()), text);
        }
        assert_eq!(Cmp::strip("1<2"), None, "only at the start");
        // the first sign is the bound's; what follows is its value, signs and all
        assert_eq!(parts("a=b=c"), (None, "a", Some((Cmp::Eq, "b=c"))));
        assert_eq!(parts("a<>b"), (None, "a", Some((Cmp::Lt, ">b"))));
    }

    #[test]
    fn what_a_comparison_holds_for() {
        let met = |cmp: Cmp| [3, 4, 5].map(|level| cmp.admits(&level, &4));
        assert_eq!(met(Cmp::Lt), [true, false, false]);
        assert_eq!(met(Cmp::Le), [true, true, false]);
        assert_eq!(met(Cmp::Eq), [false, true, false]);
        assert_eq!(met(Cmp::Ge), [false, true, true]);
        assert_eq!(met(Cmp::Gt), [false, false, true]);
        assert!(Cmp::Lt.holds(Ordering::Less) && !Cmp::Lt.holds(Ordering::Equal));
        assert!(Cmp::Lt.admits("1.9", "2.0"), "unsized values too");
        assert!(!Cmp::Le.admits(&f64::NAN, &1.0) && !Cmp::Ge.admits(&f64::NAN, &1.0));
    }

    #[test]
    fn a_scope_is_a_name_before_the_first_colon() {
        // the first colon: the rest is the value's
        assert_eq!(parts("r:yara:"), (Some("r"), "yara:", None));
        assert_eq!(parts("yara:foreign"), (Some("yara"), "foreign", None));
        // no name, a comparison or white space before it: no scope
        assert_eq!(parts(":x"), (None, ":x", None));
        assert_eq!(parts("a<b:c"), (None, "a", Some((Cmp::Lt, "b:c"))));
        assert_eq!(parts("woo bar:x"), (None, "woo bar:x", None));
        // a scope that is not the app's: the word, the colon in its text
        let term = FilterTerm::read("x:y<2");
        assert_eq!((term.scope, term.text), (Some("x"), "y"));
        let plain = term.unscoped();
        assert_eq!((plain.scope, plain.text, plain.bound), (None, "x:y", Some((Cmp::Lt, "2"))));
        assert_eq!((plain.word, plain.unscoped()), ("x:y<2", plain));
    }

    #[test]
    fn the_value_as_it_stands_and_the_whole_of_it() {
        assert_eq!(value("r:foo=bar"), "foo=bar");
        assert_eq!(value("r:>=x"), ">=x");
        assert_eq!(value("a<b"), "a<b");
        assert_eq!(value(" p: akismet < 5.0 "), "akismet < 5.0");
        assert_eq!(value("r:"), "");
        assert_eq!(whole("r:=wso-shell"), Some("wso-shell"));
        assert_eq!(whole("r:=a=b"), Some("a=b"));
        assert_eq!(whole("=x"), Some("x"));
        assert_eq!(whole("r:="), Some(""));
        // a part of the field, a text before the sign, another comparison
        assert_eq!((whole("r:wso"), whole("r:a=b"), whole("r:>=x")), (None, None, None));
    }

    #[test]
    fn a_word_still_being_typed() {
        for word in ["r:", "r:=", "s>", "s>=", "s<", "s<=", "s=", "p:akismet<", "=", "p: "] {
            assert!(FilterTerm::read(word).is_open(), "{word}");
        }
        for word in ["r", "s", "r:w", "s>=4", "r:=w", "p:akismet<5", ":", "x:y"] {
            assert!(!FilterTerm::read(word).is_open(), "{word}");
        }
    }

    #[test]
    fn the_words_of_a_filter() {
        let words: Vec<&str> = FilterTerm::words("  exampleacct   r:wso\ts>=4 ").map(|t| t.word).collect();
        assert_eq!(words, ["exampleacct", "r:wso", "s>=4"]);
        assert_eq!(FilterTerm::words("  ").count(), 0);
    }

    #[test]
    fn a_word_in_a_text_in_any_case() {
        assert!(contains_folded("wp-content/Uploads", "uploads"));
        assert!(contains_folded("x", "x") && !contains_folded("x", "xy") && !contains_folded("", "x"));
        assert!(contains_folded("anything", "") && contains_folded("", ""));
        assert!(contains_folded("Sähkö.php", "sähkö") && contains_folded("SÄHKÖ", "sähkö"));
        // the word is lowercase already: past ASCII, a capital in it is in no text
        assert!(contains_folded("uploads", "Uploads") && !contains_folded("SÄHKÖ", "SÄHKÖ"));
    }

    /* A filter of one term, as wp-scanner's: a scope of its own, the text,
       a bound that is a version. A bound that does not parse is text. */

    /// (scope, text, bound) with a number standing in for a version.
    fn one_term(input: &str) -> (&'static str, String, Option<(Cmp, u32)>) {
        let input = input.trim().to_lowercase();
        let term = FilterTerm::read(&input);
        let (scope, term) = match term.scope {
            Some("p") => ("plugin", term),
            Some("c") => ("core", term),
            _ => ("any", term.unscoped()),
        };
        match term.bound.and_then(|(cmp, v)| v.parse::<u32>().ok().map(|v| (cmp, v))) {
            Some(bound) => (scope, term.text.to_string(), Some(bound)),
            None => (scope, term.value().to_string(), None),
        }
    }

    #[test]
    fn a_filter_of_one_term() {
        assert_eq!(one_term("  "), ("any", String::new(), None));
        assert_eq!(one_term("p:Akismet<5"), ("plugin", "akismet".into(), Some((Cmp::Lt, 5))));
        assert_eq!(one_term("c:>=6"), ("core", String::new(), Some((Cmp::Ge, 6))));
        assert_eq!(one_term("woo <= 8"), ("any", "woo".into(), Some((Cmp::Le, 8))));
        // a bound that is not a version is text, and an unknown prefix is text too
        assert_eq!(one_term("a<b"), ("any", "a<b".into(), None));
        assert_eq!(one_term("x:y"), ("any", "x:y".into(), None));
        assert_eq!(one_term("p:a<b"), ("plugin", "a<b".into(), None));
    }

    /* A filter of words, as the malware scanner's: scopes with a part or
       the whole of a field, a severity bound with no colon, and a word
       still being typed asking nothing. */

    #[derive(Debug, PartialEq, Eq)]
    enum Asked {
        Text(String),
        Rule { text: String, whole: bool },
        Severity(Cmp, u8),
    }

    fn asked(text: &str) -> Vec<Asked> {
        let text = text.to_lowercase();
        FilterTerm::words(&text)
            .filter_map(|term| {
                let plain = || Some(Asked::Text(term.word.to_string()));
                if let (None, "s", Some((cmp, level))) = (term.scope, term.text, term.bound) {
                    return match level.parse::<u8>() {
                        _ if term.is_open() => None,
                        Ok(level) if (1..=5).contains(&level) => Some(Asked::Severity(cmp, level)),
                        _ => plain(),
                    };
                }
                if term.scope != Some("r") {
                    return plain();
                }
                let (whole, text) = match term.whole() {
                    Some(text) => (true, text),
                    None => (false, term.value()),
                };
                (!text.is_empty()).then(|| Asked::Rule { text: text.to_string(), whole })
            })
            .collect()
    }

    #[test]
    fn a_filter_of_words() {
        let rule = |text: &str, whole| Asked::Rule { text: text.into(), whole };
        let plain = |text: &str| Asked::Text(text.into());
        assert_eq!(asked("R:wso S>=4 Uploads"), [rule("wso", false), Asked::Severity(Cmp::Ge, 4), plain("uploads")]);
        assert_eq!(asked("r:=wso-shell r:yara: r:a=b"), [rule("wso-shell", true), rule("yara:", false), rule("a=b", false)]);
        // no scope and no bound: plain words, looked for as they stand
        assert_eq!(asked("x:1 s>=9 s>=x s s:>=4"), [plain("x:1"), plain("s>=9"), plain("s>=x"), plain("s"), plain("s:>=4")]);
        for typing in ["r:", "r:=", "s>", "s>=", "s<", "s<=", "s="] {
            assert_eq!(asked(typing), [], "{typing}");
        }
        assert_eq!(asked("uploads r:"), [plain("uploads")]);
    }
}

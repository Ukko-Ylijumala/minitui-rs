// Copyright (c) 2026 Mikko Tanner. All rights reserved.

/*!
Source text in colours: a file lexed once, when opened, into [`Token`]s,
byte ranges of the text with what they are ([`Syntax`]), and the style each
kind is shown in. What lies between the tokens is plain. From the file
viewers of wp-scanner and a sibling of it, which each had half of this.

The lexers are for the reader's eye and are nobody's verdict: a misread
costs a wrong colour until the line ends or the quote closes.

- PHP as PHP reads it: outside `<?php` / `<?=` / `<?` and `?>` the text is
  HTML, passed through, and inside, comments (`//` and `#` end at the line
  or at `?>`), strings single and double quoted, heredoc and nowdoc, with
  the variables a double-quoted string or a heredoc interpolates, backticks
  (a shell command), variables, keywords and casts, `true` / `false` /
  `null` and the magic constants, numbers, a name called as a function, and
  what follows `__halt_compiler();`, which never runs.
- CSS: comments, strings and at-rules.
- JavaScript: comments and strings, a regular expression stepped over so
  that a quote inside one opens no string.
- Perl: comments, POD, strings, and what follows `__END__` or `__DATA__`.

Every delimiter is ASCII, so in UTF-8 text each token's range starts and
ends on a character boundary; in bytes that are not UTF-8 the ranges are
those bytes'. A lexer never fails: an unclosed comment or string runs to
the end of the text, as it would for the language.

An app with a lexer of its own (a scanner showing what it saw itself)
answers in the same [`Token`]s, and has [`Syntax`]'s kinds for all a lexer
that covers every byte tells apart: markup, white space, punctuation.
*/

use std::ops::Range;
use std::path::Path;
use std::str;
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use strum::EnumString;

/* ===== consts and statics ================================================= */

static OPEN_TAG: &[u8] = b"<?";
static OPEN_PHP: &[u8] = b"php";
static CLOSE_TAG: &[u8] = b"?>";
static HEREDOC: &[u8] = b"<<<";
static BLOCK_OPEN: &[u8] = b"/*";
static BLOCK_CLOSE: &[u8] = b"*/";
static NEWLINE: &[u8] = b"\n";
/// What a magic constant starts and ends with: `__FILE__`.
static MAGIC_EDGE: &[u8] = b"__";
/// PHP reads nothing after this, called and ended.
static HALT: &str = "__halt_compiler";
/// Perl reads nothing after a line that starts with one of these.
static PERL_ENDS: [&[u8]; 2] = [b"__END__", b"__DATA__"];
/// Where Perl's POD ends: a line starting with this.
static POD_CUT: &[u8] = b"\n=cut";
/// After one of these (or white space, or nothing) a Perl `#` opens a comment.
static PERL_COMMENT_AFTER: &[u8] = b";{}(),";
/// After one of these a Perl `'` is no string: a variable's name, the old
/// package separator.
static PERL_NO_QUOTE_AFTER: &[u8] = b"$\\_";
/// After one of these a JavaScript `/` opens a regular expression: there is
/// no value to divide.
static REGEX_AFTER: &[u8] = b"(,=:[!&|?{};+-*%<>~^";

/* ===== types ============================================================== */

/**
What a text is written in.

```
use minitui::{Lang, Syntax};

let text = "<?php echo $x; // hi";
let kinds: Vec<Syntax> = Lang::of("a/b.php").tokens(text).iter().map(|t| t.kind).collect();
assert_eq!(kinds, [Syntax::Tag, Syntax::Keyword, Syntax::Variable, Syntax::Comment]);
```
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Lang {
    Php,
    Css,
    Js,
    Perl,
    /// Nothing this module reads: no tokens.
    #[default]
    Plain,
}

/// The extensions of the languages read.
#[derive(Debug, Clone, Copy, EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum Ext {
    Php,
    Phtml,
    Inc,
    Php3,
    Php4,
    Php5,
    Php7,
    Php8,
    Phps,
    Css,
    Js,
    Mjs,
    Cjs,
    Pl,
    Pm,
}

/**
What a token is, as far as a colour tells them apart.

The lexers here give only what they colour. [`Syntax::Markup`],
[`Syntax::Space`], [`Syntax::Word`], [`Syntax::Punct`] and
[`Syntax::Unknown`] are for a lexer that accounts for every byte.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Syntax {
    /// Outside the code: the HTML around PHP.
    Markup,
    /// `<?php`, `<?=`, `<?`, `?>`.
    Tag,
    Space,
    Comment,
    /// Never run: what follows `__halt_compiler();`, or Perl's `__END__`.
    Dead,
    /// A keyword, a cast, a CSS at-rule.
    Keyword,
    Variable,
    /// A name that is none of the others.
    Word,
    /// A name called: `foo(`.
    Function,
    /// `true`, `false`, `null`, `__FILE__` and the other magic constants.
    Constant,
    Number,
    /// A quoted string.
    String,
    /// A heredoc or a nowdoc.
    Heredoc,
    /// Backticks in PHP: a shell command spelled as syntax.
    Shell,
    Punct,
    /// A byte no token starts with.
    Unknown,
}

/// A run of the text, by its bytes, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: Syntax,
}

/// PHP's keywords, case-insensitive as PHP reads them.
#[derive(Debug, Clone, Copy, EnumString)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
enum Keyword {
    Abstract,
    And,
    Array,
    As,
    Break,
    Callable,
    Case,
    Catch,
    Class,
    Clone,
    Const,
    Continue,
    Declare,
    Default,
    Die,
    Do,
    Echo,
    Else,
    Elseif,
    Empty,
    Enddeclare,
    Endfor,
    Endforeach,
    Endif,
    Endswitch,
    Endwhile,
    Enum,
    Eval,
    Exit,
    Extends,
    Final,
    Finally,
    Fn,
    For,
    Foreach,
    Function,
    Global,
    Goto,
    If,
    Implements,
    Include,
    IncludeOnce,
    Instanceof,
    Insteadof,
    Interface,
    Isset,
    List,
    Match,
    Namespace,
    New,
    Or,
    Parent,
    Print,
    Private,
    Protected,
    Public,
    Readonly,
    Require,
    RequireOnce,
    Return,
    #[strum(serialize = "self")]
    SelfName,
    Static,
    Switch,
    Throw,
    Trait,
    Try,
    Unset,
    Use,
    Var,
    While,
    Xor,
    Yield,
}

/// PHP's literal constants.
#[derive(Debug, Clone, Copy, EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum Literal {
    True,
    False,
    Null,
}

/// The types a PHP cast names: `(int)`, `( string )`.
#[derive(Debug, Clone, Copy, EnumString)]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
enum Cast {
    Int,
    Integer,
    Bool,
    Boolean,
    Float,
    Double,
    Real,
    String,
    Array,
    Object,
    Unset,
    Binary,
}

/// The text being lexed, where the lexer is, and the tokens so far.
struct Lexer<'a> {
    b: &'a [u8],
    at: usize,
    out: Vec<Token>,
}

/* ===== Lang, Syntax, Token ================================================ */

impl Lang {
    /// The language of the file at `path`, by its extension. What a file is
    /// by its content (a shebang, an open tag under another name) is the
    /// app's to say.
    pub fn of(path: impl AsRef<Path>) -> Self {
        let ext = path.as_ref().extension().and_then(|e| e.to_str());
        match ext.and_then(|e| Ext::from_str(e).ok()) {
            Some(Ext::Css) => Lang::Css,
            Some(Ext::Js | Ext::Mjs | Ext::Cjs) => Lang::Js,
            Some(Ext::Pl | Ext::Pm) => Lang::Perl,
            Some(_) => Lang::Php,
            None => Lang::Plain,
        }
    }

    /// What the language is called.
    pub const fn name(self) -> &'static str {
        match self {
            Lang::Php => "PHP",
            Lang::Css => "CSS",
            Lang::Js => "JavaScript",
            Lang::Perl => "Perl",
            Lang::Plain => "plain",
        }
    }

    /// The tokens of `text` (a `&str`, or the bytes of a file that may not
    /// be UTF-8) read as this language, in order and never overlapping.
    pub fn tokens(self, text: impl AsRef<[u8]>) -> Vec<Token> {
        let mut lx = Lexer { b: text.as_ref(), at: 0, out: Vec::new() };
        match self {
            Lang::Php => lx.php(),
            Lang::Css => lx.css(),
            Lang::Js => lx.js(),
            Lang::Perl => lx.perl(),
            Lang::Plain => {}
        }
        lx.out
    }
}

impl Syntax {
    /// How the kind is shown. Code that is nothing in particular keeps the
    /// terminal's own colour, and what never runs is dim.
    pub const fn style(self) -> Style {
        match self {
            Syntax::Markup => Style::new().fg(Color::Gray),
            Syntax::Tag => Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            Syntax::Comment | Syntax::Dead => Style::new().fg(Color::DarkGray),
            Syntax::Keyword => Style::new().fg(Color::Magenta),
            Syntax::Variable => Style::new().fg(Color::Cyan),
            Syntax::Function => Style::new().fg(Color::Blue),
            Syntax::Number | Syntax::Constant => Style::new().fg(Color::Yellow),
            Syntax::String | Syntax::Heredoc => Style::new().fg(Color::Green),
            Syntax::Shell => Style::new().fg(Color::LightRed).add_modifier(Modifier::BOLD),
            Syntax::Unknown => Style::new().fg(Color::Red),
            Syntax::Space | Syntax::Word | Syntax::Punct => Style::new(),
        }
    }

    /// What a PHP name is by itself: [`Syntax::Keyword`], [`Syntax::Constant`]
    /// (`true`, `null`, `__DIR__`) or [`Syntax::Word`]. For a lexer of the
    /// app's that knows names only as names.
    pub fn php_word(word: &str) -> Self {
        if Keyword::from_str(word).is_ok() {
            Syntax::Keyword
        } else if Literal::from_str(word).is_ok() || is_magic(word.as_bytes()) {
            Syntax::Constant
        } else {
            Syntax::Word
        }
    }
}

impl Token {
    pub const fn new(range: Range<usize>, kind: Syntax) -> Self {
        Self { range, kind }
    }

    /// The token's range in its kind's style.
    pub fn styled(&self) -> (Range<usize>, Style) {
        (self.range.clone(), self.kind.style())
    }

    /**
    The ranges of `tokens` in their kinds' styles, as
    [`Visible::lines`](crate::Visible::lines) takes them.

    ```
    use minitui::{Lang, Token, Visible};

    let text = "<?php\n\techo 'hi';\n";
    let tokens = Lang::Php.tokens(text);
    let lines = Visible::new().lines(text, Token::styles(&tokens));
    assert_eq!(lines[1].to_string(), "    echo 'hi';");
    ```
    */
    pub fn styles(tokens: &[Token]) -> impl Iterator<Item = (Range<usize>, Style)> + '_ {
        tokens.iter().map(Token::styled)
    }
}

/* ===== the lexer ========================================================== */

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<u8> {
        self.b.get(self.at + ahead).copied()
    }

    fn rest(&self) -> &[u8] {
        &self.b[self.at..]
    }

    fn push(&mut self, range: Range<usize>, kind: Syntax) {
        if !range.is_empty() {
            self.out.push(Token { range, kind });
        }
    }

    /// Where the first `needle` from `from` on starts.
    fn find(&self, from: usize, needle: &[u8]) -> Option<usize> {
        self.b.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| from + i)
    }

    /// Just past the first `mark` from `from` on; the text's end without one.
    fn after(&self, from: usize, mark: &[u8]) -> usize {
        self.find(from, mark).map_or(self.b.len(), |i| i + mark.len())
    }

    /// Where the line `at` is in ends: at its newline, or the text's end.
    fn line_end(&self, at: usize) -> usize {
        self.find(at, NEWLINE).unwrap_or(self.b.len())
    }

    /// Past the white space from `from` on.
    fn space(&self, from: usize) -> usize {
        let rest = self.b.get(from..).unwrap_or_default();
        from + rest.iter().take_while(|c| c.is_ascii_whitespace()).count()
    }

    /// Where the name starting at `from` ends.
    fn name_end(&self, from: usize) -> usize {
        from + self.b[from..].iter().take_while(|&&c| is_name(c)).count()
    }

    /// Just past the quote that closes the one at `open`, a backslash taking
    /// the byte after it; `one_line` ends the string with its line at the
    /// latest. Unclosed, the text's end.
    fn quote_end(&self, open: usize, one_line: bool) -> usize {
        let quote = self.b[open];
        let mut i = open + 1;
        while i < self.b.len() {
            match self.b[i] {
                b'\\' => i += 1,
                b'\n' if one_line => return i,
                c if c == quote => return i + 1,
                _ => {}
            }
            i += 1;
        }
        self.b.len()
    }

    /// The token from the lexer to `end`, and the lexer past it.
    fn take(&mut self, end: usize, kind: Syntax) {
        self.push(self.at..end, kind);
        self.at = end;
    }

    /// `/* … */`, a docblock too; unclosed, to the end.
    fn block_comment(&mut self) {
        self.take(self.after(self.at + BLOCK_OPEN.len(), BLOCK_CLOSE), Syntax::Comment);
    }

    /* ----- PHP ------------------------------------------------------------ */

    /// HTML up to an opening tag, the code after it to `?>`; again to the end.
    fn php(&mut self) {
        while let Some(len) = self.next_open_tag() {
            self.take(self.at + len, Syntax::Tag);
            self.php_code();
        }
    }

    /// Moves to the next opening tag and returns its length; `None` at the
    /// end: the rest is HTML.
    fn next_open_tag(&mut self) -> Option<usize> {
        let mut from = self.at;
        while let Some(at) = self.find(from, OPEN_TAG) {
            self.at = at;
            let after = &self.b[at + OPEN_TAG.len()..];
            let word_ends = |n: usize| after.get(n).is_none_or(|c| c.is_ascii_whitespace());
            let php = after.get(..OPEN_PHP.len()).is_some_and(|w| w.eq_ignore_ascii_case(OPEN_PHP));
            if php && word_ends(OPEN_PHP.len()) {
                return Some(OPEN_TAG.len() + OPEN_PHP.len());
            }
            if after.first() == Some(&b'=') {
                return Some(OPEN_TAG.len() + 1);
            }
            // the short tag, `<?` and a space; not `<?xml`
            if word_ends(0) {
                return Some(OPEN_TAG.len());
            }
            from = at + OPEN_TAG.len();
        }
        self.at = self.b.len();
        None
    }

    /// Code, to the `?>` that ends it (taken) or the end.
    fn php_code(&mut self) {
        while let Some(c) = self.peek(0) {
            let next = self.peek(1);
            match c {
                b'?' if next == Some(b'>') => {
                    self.take(self.at + CLOSE_TAG.len(), Syntax::Tag);
                    return;
                }
                // an attribute, PHP 8: not a comment
                b'#' if next == Some(b'[') => self.at += 2,
                b'#' => self.line_comment(),
                b'/' if next == Some(b'/') => self.line_comment(),
                b'/' if next == Some(b'*') => self.block_comment(),
                // nothing in it is interpolated
                b'\'' => self.take(self.quote_end(self.at, false), Syntax::String),
                b'"' | b'`' => {
                    let end = self.quote_end(self.at, false);
                    let kind = if c == b'`' { Syntax::Shell } else { Syntax::String };
                    self.interpolated(self.at..end, kind);
                    self.at = end;
                }
                b'<' if self.rest().starts_with(HEREDOC) => self.heredoc(),
                b'$' if next.is_some_and(is_name_start) => {
                    self.take(self.name_end(self.at + 1), Syntax::Variable);
                }
                b'0'..=b'9' => self.number(),
                b'.' if next.is_some_and(|n| n.is_ascii_digit()) => self.number(),
                b'(' => self.cast(),
                c if is_name_start(c) => self.name(),
                _ => self.at += 1,
            }
        }
    }

    /// `//` or `#` to the end of the line, or to a `?>`, which ends the code.
    fn line_comment(&mut self) {
        let mut end = self.at;
        while end < self.b.len() && self.b[end] != b'\n' && !self.b[end..].starts_with(CLOSE_TAG) {
            end += 1;
        }
        self.take(end, Syntax::Comment);
    }

    /// `range` as a string of `kind` whose `$name`s are variables.
    fn interpolated(&mut self, range: Range<usize>, kind: Syntax) {
        let mut start = range.start;
        let mut i = range.start;
        while i < range.end {
            match self.b[i] {
                b'\\' => i += 2,
                b'$' if self.b.get(i + 1).copied().is_some_and(is_name_start) => {
                    let end = self.name_end(i + 1).min(range.end);
                    self.push(start..i, kind);
                    self.push(i..end, Syntax::Variable);
                    (start, i) = (end, end);
                }
                _ => i += 1,
            }
        }
        self.push(start..range.end, kind);
    }

    /**
    `<<<ID` (heredoc, interpolated; `"ID"` too) or `<<<'ID'` (nowdoc), its
    lines, and the line that starts with `ID` once indentation is skipped
    and no name character follows it (PHP 7.3's flexible closing).
    */
    fn heredoc(&mut self) {
        let start = self.at;
        let mut i = start + HEREDOC.len();
        while self.b.get(i).is_some_and(|&c| c == b' ' || c == b'\t') {
            i += 1;
        }
        let quote = self.b.get(i).copied().filter(|&c| c == b'\'' || c == b'"');
        if quote.is_some() {
            i += 1;
        }
        if !self.b.get(i).copied().is_some_and(is_name_start) {
            // `<<<` that opens nothing: an operator, and on
            self.at = start + HEREDOC.len();
            return;
        }
        let id_end = self.name_end(i);
        let text: &[u8] = self.b;
        let id = &text[i..id_end];
        let head_end = self.after(id_end, NEWLINE);
        self.push(start..head_end, Syntax::Heredoc);
        // the closing line: indentation, the id, no name character after it
        let mut line = head_end;
        let (body_end, close_end) = loop {
            if line >= self.b.len() {
                break (self.b.len(), self.b.len());
            }
            let indent = self.b[line..].iter().take_while(|&&c| c == b' ' || c == b'\t').count();
            let at = line + indent;
            let closes = self.b[at..].starts_with(id);
            if closes && !self.b.get(at + id.len()).copied().is_some_and(is_name) {
                break (line, at + id.len());
            }
            line = self.after(line, NEWLINE);
        };
        match quote {
            Some(b'\'') => self.push(head_end..body_end, Syntax::Heredoc),
            _ => self.interpolated(head_end..body_end, Syntax::Heredoc),
        }
        self.push(body_end..close_end, Syntax::Heredoc);
        self.at = close_end;
    }

    /// Digits, and the letters, dots and underscores of hex, binary, octal,
    /// exponents and separators.
    fn number(&mut self) {
        let part = |c: &&u8| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'.';
        self.take(self.at + self.rest().iter().take_while(part).count(), Syntax::Number);
    }

    /// `(int)`, `( string )`: a cast; any other `(`, itself.
    fn cast(&mut self) {
        let name_start = self.space(self.at + 1);
        let name_end = self.name_end(name_start);
        let close = self.space(name_end);
        let name = str::from_utf8(&self.b[name_start..name_end]);
        if self.b.get(close) == Some(&b')') && name.is_ok_and(|n| Cast::from_str(n).is_ok()) {
            self.take(close + 1, Syntax::Keyword);
        } else {
            self.at += 1;
        }
    }

    /// A name: a keyword, a constant, a function called, or none of them.
    fn name(&mut self) {
        let start = self.at;
        let end = self.name_end(start);
        self.at = end;
        // a member (`->list(`, `::class`) is no keyword, whatever its name
        let member = start >= 2 && matches!(&self.b[start - 2..start], b"->" | b"::");
        let word = str::from_utf8(&self.b[start..end]).unwrap_or_default();
        let called = self.b.get(self.space(end)) == Some(&b'(');
        let kind = match Syntax::php_word(word) {
            kind @ (Syntax::Keyword | Syntax::Constant) if !member => kind,
            _ if called => Syntax::Function,
            _ => return,
        };
        self.push(start..end, kind);
        if !member && word.eq_ignore_ascii_case(HALT) {
            self.halted();
        }
    }

    /// After `__halt_compiler`: `()` and a `;` or a `?>` end the code for
    /// good, and the rest of the text is never read.
    fn halted(&mut self) {
        let open = self.space(self.at);
        let close = self.space(open + 1);
        if self.b.get(open) != Some(&b'(') || self.b.get(close) != Some(&b')') {
            return;
        }
        let end = self.space(close + 1);
        let rest = &self.b[end..];
        if rest.starts_with(CLOSE_TAG) {
            self.push(end..end + CLOSE_TAG.len(), Syntax::Tag);
            self.at = end + CLOSE_TAG.len();
        } else if rest.first() == Some(&b';') {
            self.at = end + 1;
        } else {
            return;
        }
        self.take(self.b.len(), Syntax::Dead);
    }

    /* ----- CSS ------------------------------------------------------------ */

    /// CSS: comments, strings and at-rules.
    fn css(&mut self) {
        while let Some(c) = self.peek(0) {
            match c {
                b'/' if self.peek(1) == Some(b'*') => self.block_comment(),
                b'\'' | b'"' => self.take(self.quote_end(self.at, false), Syntax::String),
                b'@' if self.peek(1).is_some_and(is_name_start) => {
                    self.take(self.name_end(self.at + 1), Syntax::Keyword);
                }
                _ => self.at += 1,
            }
        }
    }

    /* ----- JavaScript ----------------------------------------------------- */

    /**
    JavaScript's comments and strings. A regular expression literal is told
    from a division by what stands before it and is stepped over unpainted,
    so that a quote inside one opens no string; a `'` or `"` string ends
    with its line at the latest, as the language has it.
    */
    fn js(&mut self) {
        // the last byte that was not white space, outside comments
        let mut before = b';';
        while let Some(c) = self.peek(0) {
            let at = self.at;
            let end: Option<(usize, Option<Syntax>)> = match (c, self.peek(1)) {
                (b'/', Some(b'/')) => Some((self.line_end(at), Some(Syntax::Comment))),
                (b'/', Some(b'*')) => {
                    Some((self.after(at + BLOCK_OPEN.len(), BLOCK_CLOSE), Some(Syntax::Comment)))
                }
                (b'/', _) if REGEX_AFTER.contains(&before) => self.regex_end(at).map(|end| (end, None)),
                (b'"' | b'\'', _) => Some((self.quote_end(at, true), Some(Syntax::String))),
                (b'`', _) => Some((self.quote_end(at, false), Some(Syntax::String))),
                _ => None,
            };
            match end {
                Some((end, kind)) => {
                    if let Some(kind) = kind {
                        self.push(at..end, kind);
                    }
                    // a comment is nothing to what follows it; the rest is a value
                    if kind != Some(Syntax::Comment) {
                        before = b')';
                    }
                    self.at = end;
                }
                None => {
                    if !c.is_ascii_whitespace() {
                        before = c;
                    }
                    self.at += 1;
                }
            }
        }
    }

    /// Just past the regular expression literal at `open` and its flags;
    /// `None` when the line ends first, which makes the `/` a division
    /// after all.
    fn regex_end(&self, open: usize) -> Option<usize> {
        let mut in_class = false;
        let mut i = open + 1;
        while i < self.b.len() {
            match self.b[i] {
                b'\n' => return None,
                b'\\' => i += 1,
                b'[' => in_class = true,
                b']' => in_class = false,
                b'/' if !in_class => {
                    let flags = self.b[i + 1..].iter().take_while(|c| c.is_ascii_alphabetic()).count();
                    return Some(i + 1 + flags);
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /* ----- Perl ----------------------------------------------------------- */

    /**
    Perl's comments and strings, POD as a comment and what follows `__END__`
    or `__DATA__` as never run. A `#` is a comment where one can start: at
    the head of a line, after white space or after a statement's end;
    `$#list` and `s#a#b#` are not. A quote after `$` is a variable's name
    (`$'`, `$"`), and a `'` after a letter the old package separator.
    */
    fn perl(&mut self) {
        while let Some(c) = self.peek(0) {
            let at = self.at;
            let prev = at.checked_sub(1).map(|i| self.b[i]);
            let line_start = matches!(prev, None | Some(b'\n'));
            let (end, kind) = match c {
                b'_' if line_start && PERL_ENDS.iter().any(|e| self.rest().starts_with(e)) => {
                    (self.b.len(), Syntax::Dead)
                }
                b'=' if line_start && self.peek(1).is_some_and(|n| n.is_ascii_alphabetic()) => {
                    (self.line_end(self.after(at, POD_CUT)), Syntax::Comment)
                }
                b'#' if prev.is_none_or(|p| p.is_ascii_whitespace() || PERL_COMMENT_AFTER.contains(&p)) => {
                    (self.line_end(at), Syntax::Comment)
                }
                b'"' | b'`' if prev != Some(b'$') && prev != Some(b'\\') => {
                    (self.quote_end(at, false), Syntax::String)
                }
                b'\'' if prev.is_none_or(|p| !p.is_ascii_alphanumeric() && !PERL_NO_QUOTE_AFTER.contains(&p)) => {
                    (self.quote_end(at, false), Syntax::String)
                }
                _ => {
                    self.at += 1;
                    continue;
                }
            };
            self.take(end, kind);
        }
    }
}

/* ===== free functions ===================================================== */

/// A byte a PHP name starts with: a letter, `_`, or any byte of a non-ASCII character.
fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

/// A byte of a PHP name.
fn is_name(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit()
}

/// `__FILE__`, `__DIR__` and the rest: upper case between double underscores.
fn is_magic(word: &[u8]) -> bool {
    let edge = MAGIC_EDGE.len();
    word.len() > 2 * edge
        && word.starts_with(MAGIC_EDGE)
        && word.ends_with(MAGIC_EDGE)
        && word[edge..word.len() - edge].iter().all(|c| c.is_ascii_uppercase())
}

/* ===== tests ============================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    /// Each token of `text` as `kind:text`.
    fn lexed(text: &str, lang: Lang) -> Vec<String> {
        let tokens = lang.tokens(text);
        let mut at = 0;
        for t in &tokens {
            assert!(at <= t.range.start && t.range.end <= text.len(), "in order and apart: {tokens:?}");
            at = t.range.end;
        }
        tokens.iter().map(|t| format!("{:?}:{}", t.kind, &text[t.range.clone()])).collect()
    }

    fn php(text: &str) -> Vec<String> {
        lexed(text, Lang::Php)
    }

    fn js(text: &str) -> Vec<String> {
        lexed(text, Lang::Js)
    }

    fn perl(text: &str) -> Vec<String> {
        lexed(text, Lang::Perl)
    }

    const NONE: [&str; 0] = [];

    #[test]
    fn by_extension() {
        assert_eq!(Lang::of("a/b.php"), Lang::Php);
        assert_eq!(Lang::of("x.PHTML"), Lang::Php);
        assert_eq!(Lang::of("style.css"), Lang::Css);
        assert_eq!(Lang::of("app.min.js"), Lang::Js);
        assert_eq!(Lang::of("app.MJS"), Lang::Js);
        assert_eq!(Lang::of(Path::new("lib/Mod.pm")), Lang::Perl);
        assert_eq!(Lang::of("readme.txt"), Lang::Plain);
        assert_eq!(Lang::of("LICENSE"), Lang::Plain);
        assert_eq!(Lang::default(), Lang::Plain);
        assert_eq!(Lang::Js.name(), "JavaScript");
    }

    #[test]
    fn code_between_its_tags() {
        assert_eq!(php("<p>hi</p>"), NONE, "no PHP opened: all HTML");
        assert_eq!(
            php("<b><?php echo $x; ?></b><?= $y ?>"),
            ["Tag:<?php", "Keyword:echo", "Variable:$x", "Tag:?>", "Tag:<?=", "Variable:$y", "Tag:?>"]
        );
        assert_eq!(php("<?PHP\nRETURN;"), ["Tag:<?PHP", "Keyword:RETURN"], "case-insensitive");
        assert_eq!(php("<?xml version ?><? exit;"), ["Tag:<?", "Keyword:exit"], "<?xml opens nothing");
        assert_eq!(php("<?phpx"), NONE);
        assert_eq!(php("<?ph"), NONE);
        assert_eq!(php("<?"), ["Tag:<?"]);
    }

    #[test]
    fn comments_end_at_the_line_or_the_tag() {
        assert_eq!(php("<?php // a ?> b"), ["Tag:<?php", "Comment:// a ", "Tag:?>"]);
        assert_eq!(php("<?php # a\n$b"), ["Tag:<?php", "Comment:# a", "Variable:$b"]);
        assert_eq!(php("<?php #[Attr]\n"), ["Tag:<?php"], "an attribute");
        assert_eq!(php("<?php /* ?> */ $a"), ["Tag:<?php", "Comment:/* ?> */", "Variable:$a"]);
        assert_eq!(php("<?php /** never\n closed"), ["Tag:<?php", "Comment:/** never\n closed"]);
        // a comment wedged into a call is a comment
        assert_eq!(
            php("<?php eval/**/(/**/$_POST['c']);"),
            ["Tag:<?php", "Keyword:eval", "Comment:/**/", "Comment:/**/", "Variable:$_POST", "String:'c'"]
        );
    }

    #[test]
    fn strings() {
        assert_eq!(php("<?php '?>' . 'it\\'s'"), ["Tag:<?php", "String:'?>'", "String:'it\\'s'"]);
        assert_eq!(
            php("<?php \"a $b c\\$d\" `ls $e`"),
            ["Tag:<?php", "String:\"a ", "Variable:$b", "String: c\\$d\"", "Shell:`ls ", "Variable:$e", "Shell:`"]
        );
        assert_eq!(php("<?php \"never closed $x"), ["Tag:<?php", "String:\"never closed ", "Variable:$x"]);
        assert_eq!(php("<?php 'ends on a backslash\\"), ["Tag:<?php", "String:'ends on a backslash\\"]);
        assert_eq!(php("<?php \"so does this\\"), ["Tag:<?php", "String:\"so does this\\"]);
    }

    #[test]
    fn heredoc_and_nowdoc() {
        let text = "<?php $a = <<<EOT\n  hi $name\n  EOT;\n$b = <<<'RAW'\n$not\nRAW\n?>";
        assert_eq!(
            php(text),
            [
                "Tag:<?php",
                "Variable:$a",
                "Heredoc:<<<EOT\n",
                "Heredoc:  hi ",
                "Variable:$name",
                "Heredoc:\n",
                "Heredoc:  EOT",
                "Variable:$b",
                "Heredoc:<<<'RAW'\n",
                "Heredoc:$not\n",
                "Heredoc:RAW",
                "Tag:?>",
            ]
        );
        // the closing id must not run on into a name: EOTX is the body's
        assert_eq!(php("<?php <<<EOT\nEOTX\nEOT;").last().unwrap(), "Heredoc:EOT");
        assert_eq!(php("<?php <<<EOT\nnever closed").len(), 3, "to the end");
        assert_eq!(php("<?php $a <<< 2"), ["Tag:<?php", "Variable:$a", "Number:2"], "no id: not a heredoc");
        assert_eq!(php("<?php <<<"), ["Tag:<?php"]);
        // nothing in it is a comment or a string of its own
        let text = "<?php\n$a = <<<EOT\nfirst // not a comment\nsecond 'not a string'\nEOT;\n";
        assert!(php(text).iter().all(|t| !t.starts_with("Comment") && !t.starts_with("String")));
    }

    #[test]
    fn names_numbers_and_casts() {
        assert_eq!(
            php("<?php if (TRUE) { foo( 0x1F, 1_000, .5 ); } __DIR__ null"),
            [
                "Tag:<?php",
                "Keyword:if",
                "Constant:TRUE",
                "Function:foo",
                "Number:0x1F",
                "Number:1_000",
                "Number:.5",
                "Constant:__DIR__",
                "Constant:null"
            ]
        );
        assert_eq!(
            php("<?php $x = (int) $y . ( string )$z; (x)"),
            ["Tag:<?php", "Variable:$x", "Keyword:(int)", "Variable:$y", "Keyword:( string )", "Variable:$z"]
        );
        assert_eq!(php("<?php ("), ["Tag:<?php"]);
        // a member's name is not a keyword
        assert_eq!(php("<?php $o->list(); Foo::class"), ["Tag:<?php", "Variable:$o", "Function:list"]);
        assert_eq!(php("<?php $café"), ["Tag:<?php", "Variable:$café"], "non-ASCII names");
    }

    #[test]
    fn a_php_name_by_itself() {
        assert_eq!(Syntax::php_word("ForEach"), Syntax::Keyword);
        assert_eq!(Syntax::php_word("self"), Syntax::Keyword);
        assert_eq!(Syntax::php_word("include_once"), Syntax::Keyword);
        assert_eq!(Syntax::php_word("NULL"), Syntax::Constant);
        assert_eq!(Syntax::php_word("__LINE__"), Syntax::Constant);
        assert_eq!(Syntax::php_word("__line__"), Syntax::Word);
        assert_eq!(Syntax::php_word("base64_decode"), Syntax::Word);
        assert_eq!(Syntax::php_word(""), Syntax::Word);
    }

    #[test]
    fn what_follows_the_halt_never_runs() {
        assert_eq!(
            php("<?php\necho 1;\n__HALT_COMPILER(); eval($_GET[0]); 'x' /* y */\n"),
            [
                "Tag:<?php",
                "Keyword:echo",
                "Number:1",
                "Function:__HALT_COMPILER",
                "Dead: eval($_GET[0]); 'x' /* y */\n"
            ]
        );
        assert_eq!(
            php("<?php __halt_compiler ( ) ?>\x00\x01<?php $a"),
            ["Tag:<?php", "Function:__halt_compiler", "Tag:?>", "Dead:\x00\x01<?php $a"]
        );
        // not ended, a member, or not called: code goes on
        assert_eq!(php("<?php __halt_compiler() + $a").last().unwrap(), "Variable:$a");
        assert_eq!(php("<?php $o->__halt_compiler(); $a").last().unwrap(), "Variable:$a");
        assert_eq!(php("<?php __halt_compiler; $a"), ["Tag:<?php", "Variable:$a"]);
        assert_eq!(php("<?php __halt_compiler("), ["Tag:<?php", "Function:__halt_compiler"]);
    }

    #[test]
    fn css() {
        assert_eq!(
            lexed("/*\nTheme Name: X\n*/\n@media screen { a { content: \"*/\"; } }", Lang::Css),
            ["Comment:/*\nTheme Name: X\n*/", "Keyword:@media", "String:\"*/\""]
        );
        assert!(Lang::Plain.tokens("<?php $x").is_empty());
    }

    #[test]
    fn javascript_comments_and_strings() {
        assert_eq!(
            js("var a = 'it''s'; // why\n/* a\n b */ x = \"q\\\"q\" + `t\nt`;\n"),
            [
                "String:'it'",
                "String:'s'",
                "Comment:// why",
                "Comment:/* a\n b */",
                "String:\"q\\\"q\"",
                "String:`t\nt`"
            ]
        );
        // a string ends with its line; a comment and a string run to the end
        assert_eq!(js("a = 'open\nb = 1; /* never closed"), ["String:'open", "Comment:/* never closed"]);
        assert_eq!(js(""), NONE);
    }

    #[test]
    fn a_regular_expression_opens_no_string() {
        // the quote is the expression's: nothing after it is a string
        assert_eq!(js("x = s.replace(/['\"]/g, ''); y = a / b; // c"), ["String:''", "Comment:// c"]);
        // a division: the line's quote is a string
        assert_eq!(js("y = (a) / 2 + 'k' / 3"), ["String:'k'"]);
        // a slash in a class does not end the expression, an escaped one neither
        assert_eq!(js("t = /[/']\\/'/.test(u) ? 'y' : 'n'"), ["String:'y'", "String:'n'"]);
    }

    #[test]
    fn perl_comments_strings_and_what_never_runs() {
        assert_eq!(
            perl("#!/usr/bin/perl\nmy $n = $#list; # count\nprint \"a # b\\n\", 'c';\n"),
            ["Comment:#!/usr/bin/perl", "Comment:# count", "String:\"a # b\\n\"", "String:'c'"]
        );
        // a quote that is a variable's name, the old package separator, a substitution's #
        assert_eq!(perl("$a = $'; $b = $\"; main'run(); s#x#y#;\n"), NONE);
        assert_eq!(
            perl("print 1;\n=pod\n\n'docs'\n\n=cut\nprint 2;\n__END__\n'data'\n"),
            ["Comment:=pod\n\n'docs'\n\n=cut", "Dead:__END__\n'data'\n"]
        );
        assert_eq!(perl("1;\n__DATA__\n# x").last().unwrap(), "Dead:__DATA__\n# x");
        assert_eq!(perl("=pod\nnever cut"), ["Comment:=pod\nnever cut"]);
    }

    #[test]
    fn bytes_that_are_not_utf8_keep_their_offsets() {
        // latin-1 high bytes before the comment: each is one byte of the file
        let raw: &[u8] = b"<?php $p\xe4iv\xe4 = 'p\xe4'; # \xe4\n$b;";
        let tokens = Lang::Php.tokens(raw);
        let got: Vec<(&[u8], Syntax)> = tokens.iter().map(|t| (&raw[t.range.clone()], t.kind)).collect();
        let want: [(&[u8], Syntax); 5] = [
            (b"<?php", Syntax::Tag),
            (b"$p\xe4iv\xe4", Syntax::Variable),
            (b"'p\xe4'", Syntax::String),
            (b"# \xe4", Syntax::Comment),
            (b"$b", Syntax::Variable),
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn text_cut_anywhere_is_read_to_its_end() {
        let samples: [&[u8]; 4] = [
            b"a<?php (int) $a = <<<\"EOT\"\n $b \\\nEOT;\n`ls $c\\` \"x\\$d\" #[A] # c ?><?= 0x1f . foo ( ) ?><? __halt_compiler ( ) ; z",
            b"/* a */ @media { a::after { content: 'x\\'; } } \"b",
            b"a = /[/'\\]]\\//gi; b = 'c\\\nd' + `e\\`` / 2 // f\n/* g",
            b"#!/perl\n$a = 'b\\'; \"c\\\" # d\n=pod\n\n=cut\n$#e; \n__END__\nf",
        ];
        for (lang, sample) in [Lang::Php, Lang::Css, Lang::Js, Lang::Perl].into_iter().zip(samples) {
            for cut in 0..=sample.len() {
                for text in [&sample[..cut], &sample[cut..]] {
                    let mut at = 0;
                    for t in lang.tokens(text) {
                        assert!(at <= t.range.start && t.range.start < t.range.end, "{lang:?} {text:?}");
                        at = t.range.end;
                    }
                    assert!(at <= text.len(), "{lang:?} {text:?}");
                }
            }
        }
    }

    #[test]
    fn styles() {
        // code that is nothing in particular keeps the terminal's colour
        for kind in [Syntax::Word, Syntax::Punct, Syntax::Space] {
            assert_eq!(kind.style(), Style::new());
        }
        assert_eq!(Syntax::Comment.style(), Syntax::Dead.style());
        assert_ne!(Syntax::Shell.style(), Syntax::String.style());
        let tokens = [Token::new(0..5, Syntax::Tag), Token::new(6..8, Syntax::Variable)];
        let styled: Vec<(Range<usize>, Style)> = Token::styles(&tokens).collect();
        assert_eq!(styled, [(0..5, Syntax::Tag.style()), (6..8, Syntax::Variable.style())]);
    }
}

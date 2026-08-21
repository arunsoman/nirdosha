//! A small, generic GBNF interpreter — parses a `.gbnf` grammar file into
//! rules and matches candidate strings against them. Exists for exactly
//! one purpose: `tests/fidelity.rs`'s accept/reject cross-check between
//! `compiler/nirdosha.gbnf` and the real `nirdosha` lexer+parser (unified
//! plan §4.2.3).
//!
//! **What this is not**: llama.cpp's own grammar engine. That engine
//! (reached via the `llama-cpp-gbnf` dev-dependency, in `tests/
//! fidelity.rs`) only exposes grammar *validation* (is this syntactically
//! legal, non-left-recursive GBNF with a root rule?) through its public
//! Rust binding — not string-acceptance testing, which needs a real
//! token vocabulary and a live sampler loop, a much heavier dependency
//! than a "does this file's grammar behave correctly" test should carry.
//! This module is a second, independent, general-purpose GBNF matcher
//! (it interprets whatever `.gbnf` text it's given — it isn't tuned to
//! expect `nirdosha.gbnf`'s specific rules to pass) that fills that gap.
//! It is honestly a second implementation that could itself have bugs;
//! `tests/fidelity.rs` compares its verdicts against the real compiler's
//! lexer+parser specifically so a disagreement is investigated, not
//! trusted blindly either way.
//!
//! Supports the GBNF subset `nirdosha.gbnf` actually uses: rule
//! references, quoted string literals (with `\\`/`\"`/`\n`/`\t`/`\r`
//! escapes), character classes (`[a-z]`, `[^"\\]`), alternation (`|`),
//! sequencing (juxtaposition), grouping (`(...)`), and `*`/`+`/`?`
//! repetition. Not a general GBNF implementation — no `{n,m}` counted
//! repetition, no Unicode `\u{...}` escapes, because the grammar under
//! test doesn't need them.

use std::collections::HashMap;

#[derive(Debug, Clone)]
enum Elem {
    Lit(Vec<char>),
    CharClass { negate: bool, ranges: Vec<(char, char)> },
    Ref(String),
    Seq(Vec<Elem>),
    Alt(Vec<Elem>),
    Star(Box<Elem>),
    Plus(Box<Elem>),
    Opt(Box<Elem>),
}

pub struct Grammar {
    rules: HashMap<String, Elem>,
}

struct GParser {
    chars: Vec<char>,
    pos: usize,
}

impl GParser {
    fn new(text: &str) -> Self {
        GParser { chars: text.chars().collect(), pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    /// Skips whitespace and `#`-to-end-of-line comments — never called
    /// inside a quoted literal or character class, where whitespace is
    /// significant.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_ident(&mut self) -> String {
        let start = self.pos;
        while self.peek().map(|c| c.is_alphanumeric() || c == '-' || c == '_').unwrap_or(false) {
            self.bump();
        }
        self.chars[start..self.pos].iter().collect()
    }

    /// A full grammar file: `(ident "::=" alternation)*`.
    fn parse_grammar(&mut self) -> Grammar {
        let mut rules = HashMap::new();
        loop {
            self.skip_trivia();
            if self.peek().is_none() {
                break;
            }
            let name = self.parse_ident();
            self.skip_trivia();
            assert_eq!(self.bump(), Some(':'), "expected `::=` after rule name `{name}`");
            assert_eq!(self.bump(), Some(':'), "expected `::=` after rule name `{name}`");
            assert_eq!(self.bump(), Some('='), "expected `::=` after rule name `{name}`");
            self.skip_trivia();
            let body = self.parse_alt();
            rules.insert(name, body);
        }
        Grammar { rules }
    }

    /// `alt ::= seq ("|" seq)*`
    fn parse_alt(&mut self) -> Elem {
        let mut alts = vec![self.parse_seq()];
        loop {
            self.skip_trivia();
            if self.peek() == Some('|') {
                self.bump();
                self.skip_trivia();
                alts.push(self.parse_seq());
            } else {
                break;
            }
        }
        if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Elem::Alt(alts)
        }
    }

    /// `seq ::= term*` — stops at `|`, `)`, end of rule (newline before
    /// the next rule's `ident "::="`), or end of input. This grammar's
    /// own rules are always defined on their own logical line(s), so
    /// "stop at the next top-level `ident ::=`" is approximated by
    /// stopping whenever the next token, after trivia, isn't something
    /// a `term` can start with.
    fn parse_seq(&mut self) -> Elem {
        let mut terms = Vec::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                Some('|') | Some(')') | None => break,
                _ => {}
            }
            // A rule boundary looks like `ident ::=` — peek ahead
            // without consuming unless it's actually one.
            if self.at_rule_start() {
                break;
            }
            terms.push(self.parse_term());
        }
        Elem::Seq(terms)
    }

    /// True if the parser is positioned at `ident ::=` (a new rule
    /// definition), used only to decide when the current rule's
    /// top-level sequence ends — this grammar has no other construct
    /// that could be confused with it (an `ident` alone, without `::=`
    /// following, is a rule *reference*, a valid term).
    fn at_rule_start(&self) -> bool {
        let mut p = self.pos;
        let start = p;
        while p < self.chars.len() && (self.chars[p].is_alphanumeric() || self.chars[p] == '-' || self.chars[p] == '_') {
            p += 1;
        }
        if p == start {
            return false;
        }
        let mut q = p;
        while q < self.chars.len() && self.chars[q].is_whitespace() {
            q += 1;
        }
        self.chars[q..].starts_with(&[':', ':', '='])
    }

    fn parse_term(&mut self) -> Elem {
        let atom = self.parse_atom();
        self.skip_trivia();
        match self.peek() {
            Some('*') => {
                self.bump();
                Elem::Star(Box::new(atom))
            }
            Some('+') => {
                self.bump();
                Elem::Plus(Box::new(atom))
            }
            Some('?') => {
                self.bump();
                Elem::Opt(Box::new(atom))
            }
            _ => atom,
        }
    }

    fn parse_atom(&mut self) -> Elem {
        self.skip_trivia();
        match self.peek() {
            Some('"') => self.parse_lit(),
            Some('[') => self.parse_char_class(),
            Some('(') => {
                self.bump();
                self.skip_trivia();
                let inner = self.parse_alt();
                self.skip_trivia();
                assert_eq!(self.bump(), Some(')'), "expected `)` to close group");
                inner
            }
            Some(c) if c.is_alphabetic() || c == '_' => Elem::Ref(self.parse_ident()),
            other => panic!("unexpected character in grammar: {other:?} at position {}", self.pos),
        }
    }

    fn parse_lit(&mut self) -> Elem {
        self.bump(); // opening quote
        let mut out = Vec::new();
        loop {
            match self.bump() {
                None => panic!("unterminated string literal in grammar"),
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    other => panic!("unknown escape in grammar string literal: {other:?}"),
                },
                Some(c) => out.push(c),
            }
        }
        Elem::Lit(out)
    }

    fn parse_char_class(&mut self) -> Elem {
        self.bump(); // '['
        let negate = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        loop {
            match self.peek() {
                None => panic!("unterminated character class in grammar"),
                Some(']') => {
                    self.bump();
                    break;
                }
                _ => {
                    let lo = self.class_char();
                    if self.peek() == Some('-') {
                        // Lookahead: `-` is only a range operator if
                        // followed by another class char, not the
                        // closing `]` (GBNF allows a literal trailing
                        // `-`, though this grammar never needs one).
                        let save = self.pos;
                        self.bump();
                        if self.peek() == Some(']') {
                            self.pos = save;
                            ranges.push((lo, lo));
                        } else {
                            let hi = self.class_char();
                            ranges.push((lo, hi));
                        }
                    } else {
                        ranges.push((lo, lo));
                    }
                }
            }
        }
        Elem::CharClass { negate, ranges }
    }

    fn class_char(&mut self) -> char {
        match self.bump() {
            Some('\\') => match self.bump() {
                Some('n') => '\n',
                Some('t') => '\t',
                Some('r') => '\r',
                Some('\\') => '\\',
                Some(']') => ']',
                Some(c) => c,
                None => panic!("unterminated escape in character class"),
            },
            Some(c) => c,
            None => panic!("unterminated character class in grammar"),
        }
    }
}

pub fn parse(grammar_text: &str) -> Grammar {
    GParser::new(grammar_text).parse_grammar()
}

/// Every possible end offset (into `input`, a `char` slice) after
/// matching `elem` once starting at `pos` — a small set, not a single
/// answer, because `*`/`+`/alternation are all genuinely ambiguous about
/// how much they consume (greedy matching alone can wrongly fail a
/// sequence like `"a"* "a"`, which needs backtracking to succeed). This
/// is the standard "parser combinator returns the set of possible
/// continuations" technique — simple, correct, and fast enough for a
/// test-only tool matching short program fragments.
fn match_elem(g: &Grammar, elem: &Elem, input: &[char], pos: usize) -> Vec<usize> {
    match elem {
        Elem::Lit(lit) => {
            if input[pos..].starts_with(lit.as_slice()) {
                vec![pos + lit.len()]
            } else {
                vec![]
            }
        }
        Elem::CharClass { negate, ranges } => match input.get(pos) {
            None => vec![],
            Some(&c) => {
                let in_class = ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
                if in_class != *negate {
                    vec![pos + 1]
                } else {
                    vec![]
                }
            }
        },
        Elem::Ref(name) => {
            let rule = g.rules.get(name).unwrap_or_else(|| panic!("undefined grammar rule `{name}`"));
            match_elem(g, rule, input, pos)
        }
        Elem::Seq(items) => {
            let mut positions = vec![pos];
            for item in items {
                let mut next = Vec::new();
                for &p in &positions {
                    next.extend(match_elem(g, item, input, p));
                }
                next.sort_unstable();
                next.dedup();
                positions = next;
                if positions.is_empty() {
                    break;
                }
            }
            positions
        }
        Elem::Alt(alts) => {
            let mut out = Vec::new();
            for a in alts {
                out.extend(match_elem(g, a, input, pos));
            }
            out.sort_unstable();
            out.dedup();
            out
        }
        Elem::Opt(inner) => {
            let mut out = vec![pos];
            out.extend(match_elem(g, inner, input, pos));
            out.sort_unstable();
            out.dedup();
            out
        }
        Elem::Star(inner) => repeat(g, inner, input, pos),
        Elem::Plus(inner) => {
            let mut out = Vec::new();
            for first in match_elem(g, inner, input, pos) {
                out.extend(repeat(g, inner, input, first));
            }
            out.sort_unstable();
            out.dedup();
            out
        }
    }
}

/// Zero-or-more repetitions of `inner`, breadth-first over reachable
/// offsets — bounded by `input.len()` since every accepting step
/// consumes at least one character (true for every use in this
/// grammar: no rule here can match the empty string and still be
/// wrapped in `*`/`+`).
fn repeat(g: &Grammar, inner: &Elem, input: &[char], start: usize) -> Vec<usize> {
    let mut reached = vec![start];
    let mut frontier = vec![start];
    while let Some(p) = frontier.pop() {
        for next in match_elem(g, inner, input, p) {
            if next > p && !reached.contains(&next) {
                reached.push(next);
                frontier.push(next);
            }
        }
    }
    reached
}

/// Does `input`, in its entirety, match `root`?
pub fn matches_fully(g: &Grammar, root: &str, input: &str) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let root_elem = g.rules.get(root).unwrap_or_else(|| panic!("grammar has no `{root}` rule"));
    match_elem(g, root_elem, &chars, 0).into_iter().any(|end| end == chars.len())
}

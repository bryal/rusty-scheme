// TODO: Define a formal grammar
// TODO: Instead of lexer functions returning length in source, return the source following
//       the lexed token.
// TODO: Use visitor pattern with a Tokenizer, wherein additional information can be stored,
//       such as file name.

use self::LexErr::*;
use super::SrcPos;
use itertools::Itertools;
use std::borrow::Cow;
use std::path::Path;
use std::fmt;

/// Common errors for various lexing actions
enum LexErr {
    // NOTE: For explanations of error variants, see messages in Display impl below
    UnknownEscape,
    InvalidEscapeSeq,
    UntermStr,
    UntermRawStr,
    InvalidRawStrDelim(char),
    InvalidNum,
    InvalidIdent,
    UndelimItem,
    Unexpected(&'static str),
}
impl fmt::Display for LexErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            UnknownEscape => write!(f, "Unknown character escape"),
            InvalidEscapeSeq => write!(f, "Invalid escape sequence"),
            UntermStr => write!(f, "Unterminated string literal"),
            UntermRawStr => write!(f, "Unterminated raw string literal"),
            InvalidRawStrDelim(c) => {
                write!(
                    f,
                    "Invalid character found in raw string delimitation: `{}`. Only `#` is \
                        allowed",
                    c
                )
            }
            InvalidNum => write!(f, "Invalid numeric literal"),
            InvalidIdent => write!(f, "Invalid ident"),
            UndelimItem => write!(f, "Undelimited item"),
            Unexpected(s) => write!(f, "Unexpected {}", s),
        }
    }
}

/// Unescape the character of an escape sequence.
/// E.g. `n` from the sequence `\n` unescapes to newline
fn unescape_char(c: char) -> Option<char> {
    // TODO: add more escapes
    match c {
        'n' => Some('\n'),
        't' => Some('\t'),
        '0' => Some('\0'),
        _ => None,
    }
}

/// *"A token is a structure representing a lexeme that explicitly indicates its categorization
///   for the purpose of parsing."*
/// -- [Wikipedia](https://en.wikipedia.org/wiki/Lexical_analysis#Token)
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token<'src> {
    /// Left parenthesis `(`
    LParen,
    /// Right parenthesis `)`
    RParen,
    /// Identifier
    Ident(&'src str),
    /// Numeric literal
    Num(&'src str),
    /// String literal
    Str(Cow<'src, str>),
    /// Quote (`'`)
    Quote,
}

/// Tokenize the string literal in `src` at `start`.
/// Return the unescaped literal as a `Token` and it's length,
/// including delimiting characters, in the source.
fn tokenize_str_lit<'s>(filename: &'s Path, src: &'s str, start: usize) -> (Token<'s>, usize) {
    let mut s = String::new();

    let mut chars = src[start + 1..].char_indices();

    while let Some((i, c)) = chars.next() {
        match c {
            '\n' | '\t' => continue,
            '\\' => {
                if let Some((j, e)) = chars.next() {
                    if let Some(u) = unescape_char(e) {
                        s.push(u)
                    } else {
                        SrcPos::new_pos(filename, src, start + 1 + j).error_exit(UnknownEscape)
                    }
                } else {
                    SrcPos::new_pos(filename, src, start + 1 + i).error_exit(InvalidEscapeSeq)
                }
            }
            '"' => return (Token::Str(Cow::Owned(s)), i + 2),
            _ => s.push(c),
        }
    }
    SrcPos::new_pos(filename, src, start).error_exit(UntermStr)
}

/// Tokenize the raw string literal in `src` at `start`.
/// Return the literal as a `Token` and it's length, including delimiting characters, in the source.
fn tokenize_raw_str_lit<'s>(filename: &'s Path, src: &'s str, start: usize) -> (Token<'s>, usize) {
    let str_src = &src[start + 1..];
    let n_delim_octos = str_src.chars().take_while(|&c| c == '#').count();

    if let Some(first_after_octos) = str_src[n_delim_octos..].chars().next() {
        if first_after_octos != '"' {
            SrcPos::new_pos(filename, src, start + 1 + n_delim_octos)
                .error_exit(InvalidRawStrDelim(first_after_octos))
        }
    } else {
        SrcPos::new_interval(filename, src, start, start + 1 + n_delim_octos)
            .error_exit(UntermRawStr)
    }

    let delim_octos = &str_src[..n_delim_octos];

    let str_body_src = &str_src[n_delim_octos + 1..];

    for (i, c) in str_body_src.char_indices() {
        if c == '"' && str_body_src[i + 1..].starts_with(delim_octos) {
            // octos before and after + 'r' + open and end quotes + str len
            let literal_len = n_delim_octos * 2 + 3 + i;
            return (Token::Str(Cow::Borrowed(&str_body_src[..i])), literal_len);
        }
    }
    SrcPos::new_pos(filename, src, start).error_exit(UntermRawStr)
}

/// Tokenize the numeric literal in `src` at `start`.
/// Return the `Token` and it's length in the source.
fn tokenize_num_lit<'s>(filename: &'s Path, src: &'s str, start: usize) -> (Token<'s>, usize) {
    let src_num = &src[start..];
    let mut has_decimal_pt = false;
    let mut has_e = false;
    let mut has_x = false;
    let mut prev_was_e = false;

    for (i, c) in src_num.char_indices() {
        match c {
            '_' => (),
            'E' if !has_e => {
                has_e = true;
                prev_was_e = true
            }
            'x' if !has_x => has_x = true,
            '-' if prev_was_e => (),
            _ if c.is_numeric() => (),
            '.' if !has_decimal_pt => has_decimal_pt = true,
            _ if is_delim_char(c) => return (Token::Num(&src_num[..i]), i),
            _ => break,
        }
        if c != 'E' {
            prev_was_e = false;
        }
    }
    SrcPos::new_pos(filename, src, start).error_exit(InvalidNum)
}

/// Whether `c` is a general delimiter, i.e. it delimits identifiers and numeric literals and such
fn is_delim_char(c: char) -> bool {
    match c {
        '(' | ')' | '[' | ']' | '{' | '}' | ';' => true,
        _ if c.is_whitespace() => true,
        _ => false,
    }
}

/// Returns whether `c` is a valid character of an ident
fn is_ident_char(c: char) -> bool {
    match c {
        '\'' | '"' => false,
        _ if is_delim_char(c) => false,
        _ => true,
    }
}

/// Tokenize the numeric literal in `src` at `start`.
/// Return the literal as a `Token` and it's length in the source.
fn tokenize_ident<'s>(filename: &'s Path, src: &'s str, start: usize) -> (Token<'s>, usize) {
    let src_ident = &src[start..];
    for (i, c) in src_ident.char_indices() {
        if is_delim_char(c) {
            return (Token::Ident(&src_ident[..i]), i);
        } else if !is_ident_char(c) {
            break;
        }
    }
    SrcPos::new_pos(filename, src, start).error_exit(InvalidIdent)
}

/// An iterator over the `Token`s, and their positions, of some source code
#[derive(Debug)]
struct Tokens<'s> {
    filename: &'s Path,
    src: &'s str,
    pos: usize,
}
impl<'s> Tokens<'s> {
    /// Construct a new iterator over the `Token`s of `src`
    fn new(filename: &'s Path, src: &'s str) -> Self {
        Tokens {
            filename: filename,
            src: src,
            pos: 0,
        }
    }
}
impl<'s> Iterator for Tokens<'s> {
    type Item = (Token<'s>, SrcPos<'s>);

    fn next(&mut self) -> Option<(Token<'s>, SrcPos<'s>)> {
        let pos = self.pos;
        let mut chars = self.src[pos..].char_indices().map(|(n, c)| (pos + n, c));

        while let Some((i, c)) = chars.next() {
            let (token, len) = match c {
                _ if c.is_whitespace() => continue,
                ';' => {
                    while let Some((_, c)) = chars.next() {
                        if c == '\n' {
                            break;
                        }
                    }
                    continue;
                }
                '\'' => (Token::Quote, 1),
                '(' => (Token::LParen, 1),
                ')' => (Token::RParen, 1),
                '"' => tokenize_str_lit(self.filename, self.src, i),
                'r' if self.src[i + 1..].starts_with(|c: char| c == '"' || c == '#') => {
                    tokenize_raw_str_lit(self.filename, self.src, i)
                }
                _ if c.is_numeric() => tokenize_num_lit(self.filename, self.src, i),
                _ if is_ident_char(c) => tokenize_ident(self.filename, self.src, i),
                _ => {
                    SrcPos::new_pos(self.filename, self.src, i).error_exit(Unexpected("character"))
                }
            };

            self.pos = i + len;
            return Some((
                token,
                SrcPos::new_interval(self.filename, self.src, i, self.pos),
            ));
        }
        None
    }
}

/// A tree of syntax items (Concrete Syntax Tree),
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CST<'s> {
    /// An S-Expression.
    SExpr(Vec<CST<'s>>, SrcPos<'s>),
    /// An identifier.
    Ident(&'s str, SrcPos<'s>),
    /// A numeric literal.
    Num(&'s str, SrcPos<'s>),
    /// A string literal.
    Str(Cow<'s, str>, SrcPos<'s>),
}
impl<'s> CST<'s> {
    pub fn pos(&self) -> &SrcPos<'s> {
        match *self {
            CST::SExpr(_, ref p) |
            CST::Ident(_, ref p) |
            CST::Num(_, ref p) |
            CST::Str(_, ref p) => p,
        }
    }

    /// Construct a new syntax tree from a token with a position, and the tokens following
    fn from_token((token, mut pos): (Token<'s>, SrcPos<'s>), nexts: &mut Tokens<'s>) -> Self {
        match token {
            Token::LParen => {
                let (list, end) = tokens_to_trees_until(nexts, Some((pos.clone(), &Token::RParen)));
                pos.end = end;
                CST::SExpr(list, pos)
            }
            Token::Ident(ident) => CST::Ident(ident, pos),
            Token::Num(num) => CST::Num(num, pos),
            Token::Str(s) => CST::Str(s, pos),
            Token::Quote => {
                CST::SExpr(
                    vec![
                        CST::Ident("quote", pos.clone()),
                        CST::from_token(
                            nexts.next().unwrap_or_else(
                                || pos.error_exit(Unexpected("quote")),
                            ),
                            nexts
                        ),
                    ],
                    pos,
                )
            }
            _ => pos.error_exit(Unexpected("token")),
        }
    }
}
impl<'s> fmt::Display for CST<'s> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            CST::Ident(s, _) | CST::Num(s, _) => write!(f, "{}", s),
            CST::Str(ref s, _) => write!(f, "{}", s),
            CST::SExpr(ref v, _) => {
                write!(
                    f,
                    "({})",
                    v.iter()
                        .map(|e| e.to_string())
                        .intersperse(" ".into())
                        .collect::<String>()
                )
            }
        }
    }
}


/// Construct trees from `tokens` until a lone `delim` is encountered.
///
/// Returns trees and index of closing delimiter if one was supplied.
fn tokens_to_trees_until<'s>(
    tokens: &mut Tokens<'s>,
    start_and_delim: Option<(SrcPos, &Token)>,
) -> (Vec<CST<'s>>, Option<usize>) {
    let (start, delim) = start_and_delim.map(|(s, t)| (Some(s), Some(t))).unwrap_or(
        (None, None),
    );

    let mut trees = Vec::new();

    while let Some((token, token_pos)) = tokens.next() {
        if Some(&token) == delim {
            return (trees, token_pos.end);
        } else {
            trees.push(CST::from_token((token, token_pos), tokens))
        }
    }
    match start {
        None => (trees, None),
        Some(pos) => pos.error_exit(UndelimItem),
    }
}

/// Lex the source code as a Concrete Syntax Tree
pub fn concrete_syntax_trees_from_src<'s>(filename: &'s Path, src: &'s str) -> Vec<CST<'s>> {
    tokens_to_trees_until(&mut Tokens::new(filename, src), None).0
}

#[cfg(test)]
mod test {
    use super::*;
    use super::Token::*;
}

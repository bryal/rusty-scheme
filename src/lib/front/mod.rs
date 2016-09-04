
use std::fmt::{self, Display, Debug};
use std::cmp::min;
use std::process::exit;
use std::iter::repeat;
use term;

pub mod lex;
pub mod macro_;
pub mod ast;
pub mod parse;
pub mod inference;

// All results from terminal related actions are ignored

/// A position or interval in a string of source code
#[derive(Clone, Hash, PartialEq, Eq)]
pub struct SrcPos<'src> {
    src: &'src str,
    start: usize,
    end: Option<usize>,
    in_expansion: Option<Box<SrcPos<'src>>>,
}
impl<'src> SrcPos<'src> {
    /// Construct a new `SrcPos` representing a position in `src`
    fn new_pos(src: &'src str, pos: usize) -> Self {
        SrcPos {
            src: src,
            start: pos,
            end: None,
            in_expansion: None,
        }
    }
    /// Construct a new `SrcPos` representing an interval in `src`
    fn new_interval(src: &'src str, start: usize, end: usize) -> Self {
        SrcPos {
            src: src,
            start: start,
            end: Some(end),
            in_expansion: None,
        }
    }

    fn to(&self, end: &SrcPos<'src>) -> SrcPos<'src> {
        SrcPos {
            src: self.src,
            start: self.start,
            end: Some(end.end.unwrap_or(end.start)),
            in_expansion: None,
        }
    }

    pub fn add_expansion_site(&mut self, exp: &SrcPos<'src>) {
        if self.in_expansion.is_some() {
            // if let Some(ref mut self_exp) = self.in_expansion {
            // Not sure whether this should be an error
            // panic!("Internal Compiler Error: add_expansion_site: \
            //         Tried to add expansion site `{:?}` to pos `{:?}`",
            // 	exp,
            // 	self);
            // self_exp.add_expansion_site(exp);
        } else {
            self.in_expansion = Some(Box::new(exp.clone()));
        }
    }

    fn line_len_row_col(&self) -> (&'src str, usize, usize, usize) {
        let mut line_start = 0;

        for (row, line) in self.src.lines().enumerate().map(|(n, line)| (n + 1, line)) {
            let line_len = line.len() + 1; // Include length of newline char

            if line_start <= self.start && self.start < line_start + line_len {
                let col = self.start - line_start;

                return (line, line_len, row, col);
            }
            line_start += line_len;
        }
        panic!("Internal compiler error: line_len_row_col: Pos {:?} not reached. src.len(): {}",
               self,
               self.src.len())
    }

    fn print_expansion(&self, t: &mut term::StdoutTerminal) {
        if let Some(ref exp) = self.in_expansion {
            exp.print_expansion(t);
        }

        let (line, line_len, row, col) = self.line_len_row_col();

        print!("{}:{}: ", row, col);

        t.fg(term::color::BRIGHT_MAGENTA).ok();
        println!("In expansion");
        t.reset().ok();

        println!("{}: {}", row, line);

        t.fg(term::color::BRIGHT_MAGENTA).ok();
        println!("{}^{}",
                 repeat(' ')
                     .take(col + (row as f32).log10() as usize + 3)
                     .collect::<String>(),
                 repeat('~')
                     .take(min(self.end.unwrap_or(self.start + 1) - self.start - 1,
                               line_len - col))
                     .collect::<String>());
        t.reset().ok();
    }

    /// Print an error together with information regarding position in source, and then exit.
    pub fn error<E: Display>(&self, e: E) -> ! {
        let (line, line_len, row, col) = self.line_len_row_col();
        let mut t = term::stdout().expect("Could not acquire access to stdout");

        if let Some(ref exp) = self.in_expansion {
            exp.print_expansion(&mut *t);
        }

        print!("{}:{}: ", row, col);

        t.fg(term::color::BRIGHT_RED).ok();
        print!("Error: ");
        t.reset().ok();

        println!("{}", e);
        println!("{}: {}", row, line);

        t.fg(term::color::BRIGHT_RED).ok();
        println!("{}^{}",
                 repeat(' ')
                     .take(col + (row as f32).log10() as usize + 3)
                     .collect::<String>(),
                 repeat('~')
                     .take(min(self.end.unwrap_or(self.start + 1) - self.start - 1,
                               line_len - col))
                     .collect::<String>());
        t.reset().ok();

        println!("\nError occured during compilation. Exiting\n");
        exit(0)
    }

    pub fn warn<S: Display>(&self, msg: S) {
        let (line, line_len, row, col) = self.line_len_row_col();
        let mut t = term::stdout().expect("Could not acquire access to stdout");

        if let Some(ref exp) = self.in_expansion {
            exp.print_expansion(&mut *t);
        }

        print!("{}:{}: ", row, col);

        t.fg(term::color::BRIGHT_YELLOW).ok();
        print!("Warning: ");
        t.reset().ok();

        println!("{}", msg);
        println!("{}: {}", row, line);

        t.fg(term::color::BRIGHT_YELLOW).ok();
        println!("{}^{}",
                 repeat(' ')
                     .take(col + (row as f32).log10() as usize + 3)
                     .collect::<String>(),
                 repeat('~')
                     .take(min(self.end.unwrap_or(self.start + 1) - self.start - 1,
                               line_len - col))
                     .collect::<String>());
        t.reset().ok();
    }
}
impl<'src> Debug for SrcPos<'src> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self.end {
            Some(end) => write!(fmt, "SrcPos {{ start: {}, end: {} }}", self.start, end),
            None => write!(fmt, "SrcPos {{ start: {} }}", self.start),
        }
    }
}

pub fn error<E: Display>(e: E) -> ! {
    let mut t = term::stdout().expect("Could not acquire access to stdout");

    t.fg(term::color::BRIGHT_RED).ok();
    print!("Error: ");
    t.reset().ok();

    println!("{}", e);

    println!("\nError occured during compilation. Exiting\n");
    exit(0)
}

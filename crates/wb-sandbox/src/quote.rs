//! Windows command-line quoting (the `CommandLineToArgvW` / MSVC CRT rules).
//!
//! Pure functions over UTF-16 code units so they can be unit-tested on every platform.

/// Whether `arg` must be wrapped in quotes to survive argv splitting.
fn needs_quotes(arg: &[u16]) -> bool {
    arg.is_empty()
        || arg.iter().any(|&c| {
            c == b' ' as u16
                || c == b'\t' as u16
                || c == b'\n' as u16
                || c == 0x0B
                || c == b'"' as u16
        })
}

/// Append one argument (not the program name) to `out`, quoted so that `CommandLineToArgvW`
/// and the MSVC runtime both parse it back to exactly `arg`.
///
/// Rules: 2n backslashes followed by a quote produce n backslashes and toggle quoting;
/// 2n+1 backslashes followed by a quote produce n backslashes and a literal quote;
/// backslashes not followed by a quote are literal.
pub fn append_arg(out: &mut Vec<u16>, arg: &[u16]) {
    const BACKSLASH: u16 = b'\\' as u16;
    const QUOTE: u16 = b'"' as u16;
    if !needs_quotes(arg) {
        out.extend_from_slice(arg);
        return;
    }
    out.push(QUOTE);
    let mut backslashes = 0usize;
    for &c in arg {
        if c == BACKSLASH {
            backslashes += 1;
        } else if c == QUOTE {
            // Double the pending backslashes, then escape the quote itself.
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
            out.push(QUOTE);
            backslashes = 0;
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
            out.push(c);
            backslashes = 0;
        }
    }
    // Trailing backslashes precede the closing quote, so they are doubled.
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    out.push(QUOTE);
}

/// Append the program name (argv[0]). The parsers treat it specially: it ends at the next
/// quote (if quoted) or whitespace, and backslashes are never escapes, so it is always wrapped
/// in plain quotes. A program name cannot contain a quote (not a legal file-name character).
pub fn append_program(out: &mut Vec<u16>, program: &[u16]) -> Result<(), QuoteError> {
    if program.is_empty() {
        return Err(QuoteError::EmptyProgram);
    }
    if program.contains(&(b'"' as u16)) {
        return Err(QuoteError::QuoteInProgram);
    }
    out.push(b'"' as u16);
    out.extend_from_slice(program);
    out.push(b'"' as u16);
    Ok(())
}

/// Why a command line could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteError {
    EmptyCommand,
    EmptyProgram,
    QuoteInProgram,
    /// A NUL code unit would silently truncate the command line.
    InteriorNul,
    /// Longer than the 32,767 characters `CreateProcessW` accepts.
    TooLong,
}

impl std::fmt::Display for QuoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            QuoteError::EmptyCommand => "the command is empty",
            QuoteError::EmptyProgram => "the program name is empty",
            QuoteError::QuoteInProgram => "the program name contains a double quote",
            QuoteError::InteriorNul => "an argument contains a NUL character",
            QuoteError::TooLong => "the command line exceeds 32767 characters",
        })
    }
}

impl std::error::Error for QuoteError {}

/// Maximum length of a `CreateProcessW` command line, including the terminating NUL.
pub const MAX_COMMAND_LINE: usize = 32_767;

/// Build the full command line for `argv` (program first). No terminating NUL is appended.
pub fn command_line<A: AsRef<[u16]>>(argv: &[A]) -> Result<Vec<u16>, QuoteError> {
    let (program, rest) = argv.split_first().ok_or(QuoteError::EmptyCommand)?;
    if argv.iter().any(|a| a.as_ref().contains(&0)) {
        return Err(QuoteError::InteriorNul);
    }
    let mut out = Vec::new();
    append_program(&mut out, program.as_ref())?;
    for arg in rest {
        out.push(b' ' as u16);
        append_arg(&mut out, arg.as_ref());
    }
    if out.len() >= MAX_COMMAND_LINE {
        return Err(QuoteError::TooLong);
    }
    Ok(out)
}

/// Convenience for tests and diagnostics: [`command_line`] over `&str` arguments.
pub fn command_line_str<S: AsRef<str>>(argv: &[S]) -> Result<String, QuoteError> {
    let wide: Vec<Vec<u16>> = argv
        .iter()
        .map(|s| s.as_ref().encode_utf16().collect())
        .collect();
    Ok(String::from_utf16_lossy(&command_line(&wide)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(arg: &str) -> String {
        let mut out = Vec::new();
        append_arg(&mut out, &arg.encode_utf16().collect::<Vec<_>>());
        String::from_utf16(&out).unwrap()
    }

    /// Reference parser for arguments after argv[0], following the documented post-2008 MSVC
    /// rules (which `CommandLineToArgvW` shares for these inputs).
    fn parse_args(line: &str) -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        let mut args = Vec::new();
        let mut i = 0;
        loop {
            while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let mut cur = String::new();
            let mut in_quotes = false;
            while i < chars.len() {
                let c = chars[i];
                if c == '\\' {
                    let start = i;
                    while i < chars.len() && chars[i] == '\\' {
                        i += 1;
                    }
                    let n = i - start;
                    if i < chars.len() && chars[i] == '"' {
                        cur.extend(std::iter::repeat_n('\\', n / 2));
                        if n % 2 == 1 {
                            cur.push('"');
                            i += 1;
                        }
                    } else {
                        cur.extend(std::iter::repeat_n('\\', n));
                    }
                } else if c == '"' {
                    if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                        cur.push('"');
                        i += 2;
                    } else {
                        in_quotes = !in_quotes;
                        i += 1;
                    }
                } else if (c == ' ' || c == '\t') && !in_quotes {
                    break;
                } else {
                    cur.push(c);
                    i += 1;
                }
            }
            args.push(cur);
        }
        args
    }

    #[test]
    fn plain_arguments_are_unchanged() {
        assert_eq!(quote("abc"), "abc");
        assert_eq!(quote(r"C:\dir\file.txt"), r"C:\dir\file.txt");
        assert_eq!(quote(r"trailing\"), r"trailing\");
        assert_eq!(quote("-c"), "-c");
    }

    #[test]
    fn empty_and_spaces_are_quoted() {
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("a b"), r#""a b""#);
        assert_eq!(quote("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn quotes_and_backslashes_follow_the_2n_rules() {
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote(r#"a\\"b"#), r#""a\\\\\"b""#);
        assert_eq!(quote(r"C:\Program Files\"), r#""C:\Program Files\\""#);
        assert_eq!(quote(r"a b\\"), r#""a b\\\\""#);
        assert_eq!(quote(r"a\b c"), r#""a\b c""#);
        assert_eq!(quote(r#"""#), r#""\"""#);
    }

    #[test]
    fn program_is_always_quoted_without_escapes() {
        let line = command_line_str(&[r"C:\Program Files\py\python.exe", "-I", "-m", "x"]).unwrap();
        assert_eq!(line, r#""C:\Program Files\py\python.exe" -I -m x"#);
        let line = command_line_str(&[r"C:\dir\", "a"]).unwrap();
        assert_eq!(line, r#""C:\dir\" a"#);
    }

    #[test]
    fn errors() {
        assert_eq!(command_line_str::<&str>(&[]), Err(QuoteError::EmptyCommand));
        assert_eq!(command_line_str(&[""]), Err(QuoteError::EmptyProgram));
        assert_eq!(command_line_str(&["a\"b"]), Err(QuoteError::QuoteInProgram));
        assert_eq!(
            command_line_str(&["a", "b\0c"]),
            Err(QuoteError::InteriorNul)
        );
        let long = "x".repeat(MAX_COMMAND_LINE);
        assert_eq!(
            command_line_str(&["a", long.as_str()]),
            Err(QuoteError::TooLong)
        );
    }

    #[test]
    fn round_trips_through_the_reference_parser() {
        let cases: &[&[&str]] = &[
            &["simple", "two words", ""],
            &[r#"say "hi""#, r"back\slash", r"trail\", r"trail space\"],
            &[r#"\"#, r#"\\"#, r#"\""#, r#"\\""#, r#"a\\\"b c"#],
            &[
                r#"print("a b"); x = 'c:\\d\\'"#,
                "tab\there",
                r#""""#,
                r#"" ""#,
            ],
            &[r"C:\Users\A B\scratch\", "--flag=va lue", "ünï cödé"],
        ];
        for args in cases {
            let mut argv = vec!["prog.exe"];
            argv.extend_from_slice(args);
            let line = command_line_str(&argv).unwrap();
            let rest = line.strip_prefix("\"prog.exe\"").unwrap();
            let parsed = parse_args(rest);
            assert_eq!(parsed, args.to_vec(), "line: {line}");
        }
    }
}

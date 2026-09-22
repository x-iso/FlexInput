//! A cursor that moves over a config in TOKENS, not characters.
//!
//! This is what makes a config editable from a gamepad. Every JSM line has the
//! same shape — a name, an `=`, and what it is set to — and the useful edits are
//! all "replace this whole thing": change the button, change the key it maps to,
//! change the number. A character cursor would make each of those a dozen dpad
//! presses and leave you wondering which side of a word you were on.
//!
//! So the cursor always covers exactly one token, up/down moves a line, and
//! left/right moves a token. Nothing here knows about a gamepad — it is text and
//! offsets, which is the half worth testing.

/// What a token is, so the editor can offer the right thing to replace it with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    /// Left of the `=`: a setting's name, or a button (possibly chorded).
    Name,
    /// The `=` itself.
    Equals,
    /// Right of the `=`, or a bare command's word.
    Value,
    /// `#` to end of line, as one piece.
    Comment,
}

/// One token's byte range within its line, and what it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

impl Token {
    pub fn text<'a>(&self, line: &'a str) -> &'a str {
        &line[self.start..self.end]
    }
}

/// Where the cursor is: a line, and which token on it.
///
/// A line with no tokens still has a cursor on it (at token 0) — that is how you
/// stand on a blank line to type something there.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cursor {
    pub line: usize,
    pub token: usize,
}

/// Split one line into tokens.
///
/// Whitespace separates, with two exceptions that matter for editing:
///
/// * `=` is always its own token, even written tight (`GYRO_SENS=2`), because it
///   is the structure of the line rather than part of either side.
/// * everything from `#` is one comment token, so prose isn't chopped into words
///   you have to walk through one at a time.
///
/// A chorded name (`ZL,GYRO_SENS`) stays whole: it is one trigger, and replacing
/// it wholesale is the edit people actually make.
pub fn tokenize(line: &str) -> Vec<Token> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut seen_equals = false;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i] == b'#' {
            out.push(Token { start: i, end: line.len(), kind: TokenKind::Comment });
            break;
        }
        if b[i] == b'=' {
            out.push(Token { start: i, end: i + 1, kind: TokenKind::Equals });
            seen_equals = true;
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'#' && b[i] != b'=' {
            i += 1;
        }
        // Always consume something. This loop stops at a delimiter, and every
        // delimiter is handled by a branch above that advances past it — but the
        // two are a pair, and breaking the pair spins here forever building
        // empty tokens until the allocator gives up. (Found exactly that way:
        // disabling the `=` branch turned this into a 12GB allocation.) One byte
        // of forced progress makes the failure a wrong token instead of a hang.
        if i == start {
            i += 1;
        }
        out.push(Token {
            start,
            end: i,
            kind: if seen_equals { TokenKind::Value } else { TokenKind::Name },
        });
    }
    out
}

/// The byte range of `line` within `text`, as `(start, end)` not counting the
/// newline. An out-of-range line clamps to the end, so a cursor can't point past
/// the text.
pub fn line_span(text: &str, line: usize) -> (usize, usize) {
    let mut start = 0;
    for (n, l) in text.split('\n').enumerate() {
        let end = start + l.len();
        if n == line {
            return (start, end);
        }
        start = end + 1;
    }
    (text.len(), text.len())
}

/// How many lines the text has. Always at least one — an empty config is one
/// empty line you can stand on, not nothing.
pub fn line_count(text: &str) -> usize {
    text.split('\n').count().max(1)
}

/// The tokens of the cursor's line.
pub fn tokens_at(text: &str, line: usize) -> Vec<Token> {
    let (s, e) = line_span(text, line);
    tokenize(&text[s..e])
}

/// The cursor's token, and its byte range in the WHOLE text.
///
/// `None` on a line with no tokens — a blank line, or one that is only
/// whitespace. The caller still has a valid cursor there; there is just nothing
/// under it to delete or replace.
pub fn selection(text: &str, cur: Cursor) -> Option<(Token, usize, usize)> {
    let (ls, le) = line_span(text, cur.line);
    let toks = tokenize(&text[ls..le]);
    let t = *toks.get(cur.token)?;
    Some((t, ls + t.start, ls + t.end))
}

/// Move the cursor, clamped to the text.
///
/// Horizontal movement stops at the ends of a line rather than wrapping: wrapping
/// makes "where am I" a question you have to look up, and on a pad you are
/// usually holding the stick rather than tapping it.
///
/// Vertical movement keeps the token index where it can, so running down a column
/// of settings keeps you on the values.
pub fn moved(text: &str, cur: Cursor, dx: i32, dy: i32) -> Cursor {
    let lines = line_count(text);
    let line = (cur.line as i64 + dy as i64).clamp(0, lines as i64 - 1) as usize;
    let n = tokens_at(text, line).len();
    // A line with no tokens still holds the cursor at 0.
    let last = n.saturating_sub(1);
    let token = if line != cur.line {
        // Changing line: keep the column where the new line has one.
        cur.token.min(last)
    } else {
        (cur.token as i64 + dx as i64).clamp(0, last as i64) as usize
    };
    Cursor { line, token }
}

/// Put the cursor back inside the text after an edit.
pub fn clamped(text: &str, cur: Cursor) -> Cursor {
    let line = cur.line.min(line_count(text).saturating_sub(1));
    let n = tokens_at(text, line).len();
    Cursor { line, token: cur.token.min(n.saturating_sub(1)) }
}

/// Replace the cursor's token with `with`, returning the new text.
///
/// On a line with no token under the cursor, `with` is inserted at the end of the
/// line instead — which is what "type here" means on a blank line.
pub fn replace(text: &str, cur: Cursor, with: &str) -> String {
    match selection(text, cur) {
        Some((_, s, e)) => {
            let mut out = String::with_capacity(text.len() + with.len());
            out.push_str(&text[..s]);
            out.push_str(with);
            out.push_str(&text[e..]);
            out
        }
        None => {
            let (_, le) = line_span(text, cur.line);
            let mut out = String::with_capacity(text.len() + with.len());
            out.push_str(&text[..le]);
            out.push_str(with);
            out.push_str(&text[le..]);
            out
        }
    }
}

/// What an empty slot looks like until something is put in it.
///
/// A real token, not a hidden gap: the cursor has to be able to land on it, the
/// highlight has to be able to cover it, and — the point — the parser has to be
/// able to complain about it. An unfinished line IS an error, and `?` reads as
/// one in every position ("`?` isn't a key, button or action JSM can bind"), so
/// the editor says what is missing without the editor having to know.
pub const SLOT: &str = "?";

/// Make an empty slot beside the cursor's token and move onto it.
///
/// This is how anything gets ADDED. Replacing is the common edit and the cursor
/// does that already; without a slot there would be no way to turn `N = 2` into
/// `N = 2 1` at all, because every pick would land on top of the `2`.
///
/// Nothing can follow a comment on a line, so asking for a slot after one puts
/// it before instead — the alternative is a `?` inside the comment text, which
/// is not a token and would leave the cursor pointing at nothing.
pub fn insert_slot(text: &str, cur: Cursor, after: bool) -> (String, Cursor) {
    let Some((tok, s, e)) = selection(text, cur) else {
        // A blank line: the slot is simply the line's first token.
        let (_, le) = line_span(text, cur.line);
        let mut out = String::with_capacity(text.len() + 2);
        out.push_str(&text[..le]);
        out.push_str(SLOT);
        out.push_str(&text[le..]);
        return (out, Cursor { line: cur.line, token: 0 });
    };
    let after = after && tok.kind != TokenKind::Comment;
    let (at, insert, token) = if after {
        (e, format!(" {SLOT}"), cur.token + 1)
    } else {
        (s, format!("{SLOT} "), cur.token)
    };
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..at]);
    out.push_str(&insert);
    out.push_str(&text[at..]);
    (out, Cursor { line: cur.line, token })
}

/// Delete the cursor's token, and the one space that held it apart from its
/// neighbour.
///
/// Deleting a token but leaving its spacing turns `A = B C` into `A =  C`, and a
/// few of those make a line that no longer reads like the config it is. Taking
/// the space before it (or after, at the start of a line) keeps the line tidy.
pub fn delete(text: &str, cur: Cursor) -> String {
    let Some((_, s, e)) = selection(text, cur) else { return text.to_string() };
    let (ls, _) = line_span(text, cur.line);
    let b = text.as_bytes();
    let mut from = s;
    let mut to = e;
    if from > ls && b[from - 1] == b' ' {
        from -= 1;
    } else {
        while to < b.len() && b[to] == b' ' {
            to += 1;
        }
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..from]);
    out.push_str(&text[to..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(TokenKind, &str)> {
        tokenize(line).into_iter().map(|t| (t.kind, t.text(line))).collect()
    }

    #[test]
    fn a_line_splits_into_the_pieces_you_would_edit() {
        use TokenKind::*;
        assert_eq!(
            kinds("GYRO_SENS = 2"),
            [(Name, "GYRO_SENS"), (Equals, "="), (Value, "2")]
        );
        // Written tight, the `=` is still its own piece — it is the line's
        // structure, not part of either side.
        assert_eq!(
            kinds("GYRO_SENS=2"),
            [(Name, "GYRO_SENS"), (Equals, "="), (Value, "2")]
        );
        // A chord stays whole: it is one trigger, and replacing it wholesale is
        // the edit people make.
        assert_eq!(
            kinds("ZL,GYRO_SENS = 4"),
            [(Name, "ZL,GYRO_SENS"), (Equals, "="), (Value, "4")]
        );
        // Two outputs are two tokens, so either can be changed on its own.
        assert_eq!(
            kinds("N = 2 1"),
            [(Name, "N"), (Equals, "="), (Value, "2"), (Value, "1")]
        );
        // A comment is one piece, not a walk through its words.
        assert_eq!(
            kinds("S = SPACE   # jump, obviously"),
            [(Name, "S"), (Equals, "="), (Value, "SPACE"), (Comment, "# jump, obviously")]
        );
        assert_eq!(kinds("RESET_MAPPINGS"), [(Name, "RESET_MAPPINGS")]);
        assert!(kinds("").is_empty());
        assert!(kinds("    ").is_empty());
        assert_eq!(kinds("# just a note"), [(Comment, "# just a note")]);
    }

    /// Tokenising always finishes, whatever it is handed.
    ///
    /// The scan stops at delimiters that other branches consume, so the two are a
    /// pair — and breaking the pair used to spin forever building empty tokens
    /// until the allocator gave up. Every byte must move the cursor on.
    #[test]
    fn tokenizing_always_terminates_and_covers_the_line() {
        for line in [
            "", "=", "==", "###", "= = =", "a=b=c", "		", "#", "A=", "=B",
            "N = 2 1 # c", "ZL,GYRO_SENS=4#x", "naïve = — # em dash",
        ] {
            let toks = tokenize(line);
            // Strictly increasing and inside the line: no empty or overlapping
            // tokens, which is what a stalled scan produces.
            let mut prev_end = 0usize;
            for t in &toks {
                assert!(t.end > t.start, "empty token in {line:?}: {t:?}");
                assert!(t.start >= prev_end, "tokens overlap in {line:?}");
                assert!(t.end <= line.len(), "token past the end of {line:?}");
                prev_end = t.end;
            }
            // Everything that isn't whitespace is inside some token.
            let covered: usize = toks.iter().map(|t| t.end - t.start).sum();
            let non_ws = line.bytes().filter(|c| !c.is_ascii_whitespace()).count();
            assert!(covered >= non_ws, "{line:?} lost characters: {toks:?}");
        }
    }

    #[test]
    fn the_cursor_walks_tokens_and_lines_without_falling_off() {
        let text = "GYRO_SENS = 2\n\nS = SPACE\n";
        let c = Cursor::default();
        assert_eq!(moved(text, c, -1, 0), c, "left at the start stays put");
        let c = moved(text, c, 1, 0);
        assert_eq!(c.token, 1);
        let c = moved(text, c, 1, 0);
        assert_eq!(c.token, 2);
        assert_eq!(moved(text, c, 1, 0).token, 2, "and stops at the end of the line");

        // A blank line holds the cursor at token 0 — that is where you stand to
        // type something new.
        let blank = moved(text, c, 0, 1);
        assert_eq!(blank, Cursor { line: 1, token: 0 });
        assert!(selection(text, blank).is_none(), "nothing under it yet");

        // Going down keeps the column where the new line is long enough.
        let back = moved(text, Cursor { line: 0, token: 2 }, 0, 2);
        assert_eq!(back, Cursor { line: 2, token: 2 }, "still on the value");

        // The last line is the floor; a trailing newline makes an empty one.
        let bottom = moved(text, back, 0, 99);
        assert_eq!(bottom.line, line_count(text) - 1);
        assert_eq!(moved(text, bottom, 0, 1), bottom);
        assert_eq!(moved(text, Cursor::default(), 0, -1), Cursor::default());
    }

    #[test]
    fn the_selection_points_at_the_right_bytes_of_the_whole_text() {
        let text = "GYRO_SENS = 2\nS = SPACE\n";
        let (t, s, e) = selection(text, Cursor { line: 1, token: 2 }).expect("a token");
        assert_eq!(&text[s..e], "SPACE");
        assert_eq!(t.kind, TokenKind::Value);
        // Non-ASCII in a comment must not split a character.
        let odd = "S = SPACE # naïve — em dash\n";
        let (_, s, e) = selection(odd, Cursor { line: 0, token: 3 }).expect("the comment");
        assert_eq!(&odd[s..e], "# naïve — em dash");
    }

    #[test]
    fn replacing_a_token_leaves_the_rest_of_the_line_alone() {
        let text = "GYRO_SENS = 2   # feel\nS = SPACE\n";
        let out = replace(text, Cursor { line: 0, token: 2 }, "4");
        assert_eq!(out, "GYRO_SENS = 4   # feel\nS = SPACE\n", "spacing and comment survive");

        // On a blank line there is nothing to replace, so it is typed there.
        let blank = "A = B\n\nC = D\n";
        assert_eq!(
            replace(blank, Cursor { line: 1, token: 0 }, "GYRO_SENS = 2"),
            "A = B\nGYRO_SENS = 2\nC = D\n"
        );
    }

    #[test]
    fn deleting_a_token_takes_the_space_that_held_it() {
        let text = "N = 2 1\n";
        assert_eq!(delete(text, Cursor { line: 0, token: 3 }), "N = 2\n", "the second output");
        assert_eq!(delete(text, Cursor { line: 0, token: 2 }), "N = 1\n", "or the first");
        // Deleting the name takes the space AFTER it, since there is none before.
        assert_eq!(delete(text, Cursor { line: 0, token: 0 }), "= 2 1\n");
        // Nothing under the cursor is not an edit.
        let blank = "A = B\n\n";
        assert_eq!(delete(blank, Cursor { line: 1, token: 0 }), blank);
    }
}

#[cfg(test)]
mod delete_round_tests {
    use super::*;

    /// Deleting from a pad is the one edit that loses work, so it has to leave a
    /// config you would recognise — and leave the cursor somewhere real.
    #[test]
    fn deleting_walks_a_line_down_without_stranding_the_cursor() {
        let mut text = "N = 2 1 # both\n".to_string();
        let mut cur = Cursor { line: 0, token: 3 };

        // Hold the chord and the line comes apart from the cursor outwards. The
        // index stays where it was, so once the second output goes the cursor is
        // on the comment — which is exactly what the highlight shows you, and
        // why the cursor is drawn rather than assumed.
        for expect in ["N = 2 # both\n", "N = 2\n", "N =\n", "N\n"] {
            text = delete(&text, cur);
            assert_eq!(text, expect, "after deleting token {}", cur.token);
            cur = clamped(&text, cur);
            // Whatever is left, the cursor points at a token that exists.
            if !tokens_at(&text, cur.line).is_empty() {
                assert!(
                    selection(&text, cur).is_some(),
                    "cursor {cur:?} is off the end of {text:?}"
                );
            }
        }
        // The name is the last thing standing, and deleting it empties the line
        // rather than the file.
        text = delete(&text, clamped(&text, cur));
        assert_eq!(text, "\n", "the line is still there, just empty");
        assert!(selection(&text, Cursor::default()).is_none());
        // Deleting again on an empty line is not an edit — a held chord must not
        // eat the rest of the config.
        assert_eq!(delete(&text, Cursor::default()), "\n");
    }

    /// The lines around the one being edited are untouched, byte for byte.
    #[test]
    fn deleting_a_token_leaves_the_neighbouring_lines_alone() {
        let text = "A = B\nGYRO_SENS = 2\nC = D\n";
        let out = delete(text, Cursor { line: 1, token: 2 });
        assert_eq!(out, "A = B\nGYRO_SENS =\nC = D\n");
        assert!(out.starts_with("A = B\n") && out.ends_with("C = D\n"));
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    /// A slot is how anything gets ADDED: the cursor replaces, so without one
    /// there is no way to turn `N = 2` into `N = 2 1` from a pad at all.
    #[test]
    fn a_slot_opens_beside_the_cursor_and_takes_it() {
        let text = "N = 2\n";
        let (out, cur) = insert_slot(text, Cursor { line: 0, token: 2 }, true);
        assert_eq!(out, "N = 2 ?\n");
        assert_eq!(cur, Cursor { line: 0, token: 3 }, "the cursor moves onto the slot");
        assert_eq!(selection(&out, cur).unwrap().0.kind, TokenKind::Value);

        // Before, for an output that belongs first.
        let (out, cur) = insert_slot(text, Cursor { line: 0, token: 2 }, false);
        assert_eq!(out, "N = ? 2\n");
        assert_eq!(cur, Cursor { line: 0, token: 2 }, "the slot takes the index");

        // And filling it is the ordinary replace.
        assert_eq!(replace(&out, cur, "1"), "N = 1 2\n");
    }

    /// Nothing can follow a comment on a line, so "after" a comment means before
    /// it — a `?` inside the comment text is not a token, and the cursor would
    /// be left pointing at nothing.
    #[test]
    fn a_slot_asked_for_after_a_comment_lands_before_it() {
        let text = "S = SPACE # jump\n";
        let comment = Cursor { line: 0, token: 3 };
        assert_eq!(selection(text, comment).unwrap().0.kind, TokenKind::Comment);

        let (out, cur) = insert_slot(text, comment, true);
        assert_eq!(out, "S = SPACE ? # jump\n");
        assert_eq!(cur, comment, "the slot took the comment's index");
        assert_eq!(selection(&out, cur).unwrap().0.text(&out[..out.len() - 1]), "?");
    }

    /// On a blank line the slot IS the line — that is how you start a new
    /// setting on the empty line you just moved onto.
    #[test]
    fn a_slot_on_a_blank_line_becomes_its_first_token() {
        let text = "A = B\n\nC = D\n";
        let blank = Cursor { line: 1, token: 0 };
        assert!(selection(text, blank).is_none(), "nothing there to begin with");

        let (out, cur) = insert_slot(text, blank, true);
        assert_eq!(out, "A = B\n?\nC = D\n");
        assert_eq!(cur, blank);
        assert!(selection(&out, cur).is_some(), "and now there is");
        // The neighbours are untouched.
        assert!(out.starts_with("A = B\n") && out.ends_with("C = D\n"));
    }

    /// A slot opened and deleted leaves the line exactly as it was — a pad user
    /// who opens one by mistake gets their config back, not a stray space.
    #[test]
    fn opening_a_slot_and_deleting_it_is_a_round_trip() {
        for (text, cur) in [
            ("N = 2 1\n", Cursor { line: 0, token: 2 }),
            ("GYRO_SENS = 4   # feel\n", Cursor { line: 0, token: 0 }),
            ("S = SPACE # jump\n", Cursor { line: 0, token: 2 }),
        ] {
            for after in [true, false] {
                let (opened, at) = insert_slot(text, cur, after);
                assert_eq!(delete(&opened, at), text, "{text:?} after={after}");
            }
        }
    }
}

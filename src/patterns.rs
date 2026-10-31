//! Recognising the statement shapes the recovered BASIC is written with.
//!
//! These are fixed grammars over short lines, so they are matched directly
//! rather than compiled: the pattern layer runs on every statement of every
//! pass and compiling a regex set for it costs more than the matching does.

/// One recognised statement shape.
#[derive(Debug, PartialEq, Eq)]
pub enum Shape<'a> {
    /// `' ==== source line N ====`
    LineMarker(u32),
    /// `L00097E:`
    Label(&'a str),
    /// `FOR v = lo TO hi [STEP s]`
    For {
        var: &'a str,
        lo: &'a str,
        hi: &'a str,
        step: Option<&'a str>,
    },
    /// `IF <cond> <> 0 THEN [GOTO L]`, or the `CASE` form
    /// `IF <cond> = <n> THEN GOTO L`.
    If {
        cond: &'a str,
        op: &'a str,
        /// The value compared against, for the `CASE` form.
        value: Option<&'a str>,
        target: Option<&'a str>,
    },
    /// `NEXT` or `NEXT v` -- but not `NEXT ARRAY INDEX(a)`.
    Next,
    /// `arr(index)`, or `arr(index).field`
    ArrayDst {
        name: &'a str,
        index: &'a str,
        field: Option<&'a str>,
    },
    /// A header line of `recovered.dba`, which is replaced, not repeated.
    Banner,
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Is `i` in `s` the start of `\s+TO\s+` (at least one space each side)?
fn to_keyword(s: &str, i: usize) -> bool {
    let b = s.as_bytes();
    if !s[i..].starts_with("TO") {
        return false;
    }
    let before_ws = i > 0 && b[i - 1] == b' ';
    let after = i + 2;
    let after_ws = b.get(after) == Some(&b' ');
    before_ws && after_ws
}

/// Split a `FOR` line into its variable, bounds and step.
fn for_parts(s: &str) -> Option<(&str, &str, &str, Option<&str>)> {
    let rest = s.strip_prefix("FOR")?.trim_start();
    let (var, rest) = rest.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    // `(.+?)\s+TO\s+(.+?)` -- the shortest bounds that leave a `TO` behind.
    let to = (1..rest.len()).find(|&i| rest.is_char_boundary(i) && to_keyword(rest, i))?;
    let lo = rest[..to].trim_end();
    if lo.is_empty() {
        return None;
    }
    let hi_all = rest[to + 2..].trim_start();
    // `(.+?)(?:\s+STEP\s+(.+))?$` -- the step, when there is one, ends it.
    if let Some(step_at) = find_step(hi_all) {
        let hi = hi_all[..step_at].trim_end();
        let step = hi_all[step_at + 5..].trim_start();
        if !hi.is_empty() && !step.is_empty() {
            return Some((var, lo, hi, Some(step)));
        }
    }
    (!hi_all.is_empty()).then_some((var, lo, hi_all, None))
}

/// The first `\s+STEP\s+` in `s`.
fn find_step(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = s.get(from..)?.find("STEP") {
        let at = from + at;
        if at > 0
            && s.as_bytes()[at - 1] == b' '
            && s.get(at + 4..).is_some_and(|r| r.starts_with(' '))
        {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Split an `IF ... THEN [GOTO label]` line into its test and branch.
///
/// The condition is the shortest prefix that leaves `<> 0 THEN` (or
/// `= 0 THEN`) immediately behind, so `a > b <> 0` tests `a > b`.
fn if_parts(s: &str) -> Option<(&str, &str, Option<&str>, Option<&str>)> {
    let rest = s.strip_prefix("IF")?.trim_start();
    for i in (1..rest.len()).filter(|i| rest.is_char_boundary(*i)) {
        let cond = rest[..i].trim_end();
        if cond.is_empty() {
            continue;
        }
        if let Some((op, value, target)) = zero_then(rest[i..].trim_start()) {
            return Some((cond, op, value, target));
        }
    }
    None
}

/// Does `s` begin with `<> 0 THEN`, `= 0 THEN` or `= <n> THEN` (the `CASE`
/// form), optionally followed by `GOTO <label>`?
fn zero_then<'a>(s: &'a str) -> Option<(&'static str, Option<&'a str>, Option<&'a str>)> {
    for op in ["<>", "="] {
        let Some(head) = s.strip_prefix(op) else {
            continue;
        };
        let head = head.strip_prefix(' ')?;
        // the compared value: `0`, or the case value
        let end = head
            .find(' ')
            .filter(|e| *e > 0 && !head[..*e].is_empty())
            .unwrap_or(head.len());
        let value = &head[..end];
        let is_number = !value.is_empty()
            && value
                .trim_start_matches('-')
                .bytes()
                .all(|b| b.is_ascii_digit());
        if !is_number {
            continue;
        }
        let head = head[end..].strip_prefix(' ')?;
        let head = head.strip_prefix("THEN")?;
        // `<> 0` / `= 0` is the boolean form: there is no case value
        let value = if value == "0" { None } else { Some(value) };
        if head.is_empty() {
            return Some((op, value, None));
        }
        let label = head.strip_prefix(" GOTO ")?.trim();
        if label.is_empty() || label.contains(char::is_whitespace) {
            continue;
        }
        return Some((op, value, Some(label)));
    }
    None
}

/// Recognise one line of the transcription, if it has a shape of its own.
pub fn shape(s: &str) -> Option<Shape<'_>> {
    if let Some(rest) = s.strip_prefix("' ==== source line ")
        && let Some(n) = rest.strip_suffix(" ====")
        && digits(n)
    {
        return Some(Shape::LineMarker(n.parse().unwrap_or(0)));
    }
    if let Some(rest) = s.strip_prefix('\'') {
        if is_banner_rest(rest) {
            return Some(Shape::Banner);
        }
        return None;
    }
    if let Some(target) = s.strip_suffix(':')
        && target.len() > 1
        && target.starts_with('L')
        && target[1..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase())
    {
        return Some(Shape::Label(target));
    }
    if s == "NEXT"
        || s.strip_prefix("NEXT ")
            .is_some_and(|r| is_identifierish(r) && !r.contains('('))
    {
        return Some(Shape::Next);
    }
    if s.starts_with("IF ")
        && let Some((cond, op, value, target)) = if_parts(s)
    {
        return Some(Shape::If {
            cond,
            op,
            value,
            target,
        });
    }
    if let Some((var, lo, hi, step)) = for_parts(s) {
        return Some(Shape::For { var, lo, hi, step });
    }
    if let Some((name, idx, field)) = array_dst(s) {
        return Some(Shape::ArrayDst { name, index: idx, field });
    }
    None
}

/// `[A-Za-z_]\w*\$?\w*`, as the array-destination pattern spells it.
fn is_identifierish(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    // `[A-Za-z_]\w*\$?\w*`: one `$` may sit anywhere after the first letter.
    let dollars = s[1..].bytes().filter(|b| *b == b'$').count();
    dollars <= 1
        && s[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

/// `name(index)`
fn array_dst(s: &str) -> Option<(&str, &str, Option<&str>)> {
    let open = s.find('(')?;
    let name = &s[..open];
    let rest = &s[open + 1..];
    // `arr(i)` or `arr(i).12` -- the field offset the compiler writes after an
    // array element store
    let close = rest.find(')')?;
    let inner = &rest[..close];
    let tail = &rest[close + 1..];
    let field = if tail.is_empty() {
        None
    } else {
        Some(tail.strip_prefix('.')?)
    };
    if field.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    (is_identifierish(name) && !inner.is_empty() && !inner.contains(')')).then_some((
        name, inner, field,
    ))
}

/// The first line of `recovered.dba`'s own header.
fn is_banner_rest(rest: &str) -> bool {
    let Some(rest) = rest.strip_prefix(' ') else {
        return false;
    };
    for w in [
        "DarkBASIC PRO",
        "source blob",
        "machine code",
        "commands",
        "expressions",
        "control flow",
        "statements",
        "gaps",
        "Names:",
        "par<n>",
        "loc<disp>",
        "variable-space",
    ] {
        // The banner pattern is anchored only at the start: the rest of the
        // line is the header's own text.
        if rest.starts_with(w) {
            return true;
        }
    }
    // `'=+$` / `'-+$`: the rule lines that close the banner.
    !rest.is_empty()
        && rest.len() >= 3
        && rest.bytes().all(|b| b == b'=' || b == b'-')
        && rest.bytes().all(|b| b == rest.as_bytes()[0])
}

/// Is `text` (a whole `dst = value` line) an initialiser assignment?
pub fn is_init_text(text: &str) -> bool {
    let Some((lhs, rhs)) = text.split_once(" = ") else {
        return false;
    };
    if !is_identifierish(lhs.strip_prefix('$').unwrap_or(lhs)) {
        return false;
    }
    rhs == "\"\""
        || (rhs
            .strip_prefix('-')
            .unwrap_or(rhs)
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
            && rhs.bytes().any(|b| b.is_ascii_digit() || b == b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_line_markers() {
        assert_eq!(
            shape("' ==== source line 42 ===="),
            Some(Shape::LineMarker(42))
        );
        assert_eq!(shape("' ==== source line x ===="), None);
    }

    #[test]
    fn reads_labels() {
        assert_eq!(shape("L00097E:"), Some(Shape::Label("L00097E")));
        assert_eq!(shape("Labc:"), None);
        assert_eq!(shape("n1:"), None);
    }

    #[test]
    fn reads_for_loops() {
        assert_eq!(for_parts("FOR i = 1 TO 10"), Some(("i", "1", "10", None)));
        assert_eq!(
            for_parts("FOR i = a + 1 TO n * 2 STEP 3"),
            Some(("i", "a + 1", "n * 2", Some("3")))
        );
        assert_eq!(for_parts("FORMAT TEXT"), None);
    }

    #[test]
    fn reads_conditions_left_to_right() {
        assert_eq!(
            if_parts("IF a > b <> 0 THEN"),
            Some(("a > b", "<>", None, None))
        );
        assert_eq!(
            if_parts("IF a > b <> 0 THEN GOTO L00097E"),
            Some(("a > b", "<>", None, Some("L00097E")))
        );
        assert_eq!(if_parts("IF a = 0 THEN"), Some(("a", "=", None, None)));
        // a `CASE` test compares against its own value
        assert_eq!(
            if_parts("IF a = 7 THEN GOTO L1"),
            Some(("a", "=", Some("7"), Some("L1")))
        );
        assert_eq!(if_parts("IF a THEN"), None);
    }

    #[test]
    fn reads_array_destinations() {
        assert_eq!(array_dst("arr$3(2)"), Some(("arr$3", "2", None)));
        assert_eq!(array_dst("arr$3(2).12"), Some(("arr$3", "2", Some("12"))));
        assert_eq!(array_dst("arr$3(2).x"), None);
        assert_eq!(array_dst("n3(x + 1)"), Some(("n3", "x + 1", None)));
        assert_eq!(array_dst("3(x)"), None);
    }

    #[test]
    fn next_is_a_closer_unless_it_takes_arguments() {
        assert_eq!(shape("NEXT"), Some(Shape::Next));
        assert_eq!(shape("NEXT i"), Some(Shape::Next));
        assert_eq!(shape("NEXT ARRAY INDEX(a)"), None);
    }

    #[test]
    fn recognises_the_recovered_file_header() {
        assert_eq!(shape("' machine code: 2461 bytes"), Some(Shape::Banner));
        assert_eq!(shape("' ======"), Some(Shape::Banner));
        assert_eq!(shape("' a note"), None);
    }

    #[test]
    fn recognises_initialiser_lines() {
        assert!(is_init_text("n4 = 0"));
        assert!(is_init_text("score$ = \"\""));
        assert!(!is_init_text("n4 = a + 1"));
        assert!(!is_init_text("FOR i = 1 TO 3"));
    }
}

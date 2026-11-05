//! What a statement's text says before it reaches the server.
//!
//! An editor sends SQL the way a person wrote it: comments first, a `;` or a
//! SQL*Plus `/` last. A `;` is a syntax error (ORA-00933) on a plain statement
//! before 23ai and required at the end of PL/SQL, and `oracle-rs` classifies a
//! statement by its first word, which a leading comment hides.

/// How a statement touches the session's transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionEffect {
    /// `SAVEPOINT` and `SET TRANSACTION`: the user is managing this transaction.
    Opens,
    /// `COMMIT` and `ROLLBACK`.
    Closes,
    /// Anything else, including `ROLLBACK TO`, which stays in the transaction.
    None,
}

/// The text to send to the server: no leading comments, no terminator unless
/// PL/SQL needs it, and nothing after it.
pub fn executable_sql(sql: &str) -> &str {
    let start = leading_trivia_len(sql);
    let body = &sql[start..];
    let plsql = is_plsql(body);
    let mut end = significant_end(body);
    loop {
        let text = body[..end].trim_end();
        if plsql && text.ends_with(';') {
            return text;
        }
        if text.ends_with(';') {
            end = text.len() - 1;
        } else if let Some(rest) = strip_sqlplus_slash(text) {
            end = rest.len();
        } else {
            return text;
        }
    }
}

pub fn transaction_effect(sql: &str) -> TransactionEffect {
    let words = leading_words(sql, 3);
    match words.first().map(String::as_str) {
        Some("COMMIT") => TransactionEffect::Closes,
        Some("ROLLBACK") => {
            let to = match words.get(1).map(String::as_str) {
                Some("WORK") => words.get(2).map(String::as_str),
                other => other,
            };
            if to == Some("TO") {
                TransactionEffect::None
            } else {
                TransactionEffect::Closes
            }
        }
        Some("SAVEPOINT") => TransactionEffect::Opens,
        Some("SET") if words.get(1).map(String::as_str) == Some("TRANSACTION") => {
            TransactionEffect::Opens
        }
        _ => TransactionEffect::None,
    }
}

pub fn is_explain_plan(sql: &str) -> bool {
    let words = leading_words(sql, 3);
    words == ["EXPLAIN", "PLAN", "FOR"] || words == ["EXPLAIN", "PLAN", "SET"]
}

fn is_plsql(body: &str) -> bool {
    let words = leading_words(body, 5);
    let mut words = words.iter().map(String::as_str).peekable();
    match words.next() {
        Some("BEGIN" | "DECLARE") => true,
        Some("CREATE") => {
            if words.peek() == Some(&"OR") {
                words.next();
                words.next();
            }
            if matches!(words.peek(), Some(&"EDITIONABLE" | &"NONEDITIONABLE")) {
                words.next();
            }
            matches!(
                words.next(),
                Some("FUNCTION" | "LIBRARY" | "PACKAGE" | "PROCEDURE" | "TRIGGER" | "TYPE")
            )
        }
        _ => false,
    }
}

/// A `/` alone on the last line runs the buffer in SQL*Plus; it isn't SQL.
fn strip_sqlplus_slash(text: &str) -> Option<&str> {
    let rest = text.strip_suffix('/')?;
    let line_start = rest.rfind('\n').map_or(0, |index| index + 1);
    rest[line_start..]
        .trim()
        .is_empty()
        .then_some(rest)
        .filter(|rest| !rest.trim().is_empty())
}

fn leading_words(sql: &str, limit: usize) -> Vec<String> {
    let mut rest = &sql[leading_trivia_len(sql)..];
    let mut words = Vec::with_capacity(limit);
    while words.len() < limit {
        let end = rest
            .find(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .unwrap_or(rest.len());
        if end == 0 {
            break;
        }
        words.push(rest[..end].to_ascii_uppercase());
        rest = &rest[end..];
        rest = &rest[leading_trivia_len(rest)..];
    }
    words
}

/// Whitespace and comments at the start of `sql`, in bytes.
fn leading_trivia_len(sql: &str) -> usize {
    let mut index = 0;
    loop {
        let rest = &sql[index..];
        let trimmed = rest.trim_start();
        index += rest.len() - trimmed.len();
        if trimmed.starts_with("--") {
            index += trimmed.find('\n').map_or(trimmed.len(), |end| end + 1);
        } else if let Some(comment) = trimmed.strip_prefix("/*") {
            index += comment.find("*/").map_or(trimmed.len(), |end| end + 4);
        } else {
            return index;
        }
    }
}

/// Where the last token ends, skipping comments and whitespace after it. Walks
/// the text so a `--` or `;` inside a string or quoted name doesn't count.
fn significant_end(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut index = 0;
    let mut end = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index = sql[index..].find('\n').map_or(bytes.len(), |at| index + at);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = sql[index + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |at| index + 2 + at + 2);
                continue;
            }
            b'\'' => index = quoted_end(sql, index, q_quote_at(bytes, index)),
            b'"' => {
                index = sql[index + 1..]
                    .find('"')
                    .map_or(bytes.len(), |at| index + 1 + at + 1);
            }
            byte if byte.is_ascii_whitespace() => {
                index += 1;
                continue;
            }
            _ => index += 1,
        }
        end = index;
    }
    end
}

/// The closing delimiter of a `q'[...]'` literal opening at the quote at `at`.
fn q_quote_at(bytes: &[u8], at: usize) -> Option<u8> {
    let prefix = at.checked_sub(1).map(|index| bytes[index])?;
    if !prefix.eq_ignore_ascii_case(&b'q') {
        return None;
    }
    let boundary = at < 2 || !bytes[at - 2].is_ascii_alphanumeric() || bytes[at - 2] == b'n';
    if !boundary {
        return None;
    }
    bytes.get(at + 1).map(|open| match open {
        b'[' => b']',
        b'(' => b')',
        b'{' => b'}',
        b'<' => b'>',
        other => *other,
    })
}

fn quoted_end(sql: &str, at: usize, q_close: Option<u8>) -> usize {
    let bytes = sql.as_bytes();
    if let Some(close) = q_close {
        let mut index = at + 2;
        while index + 1 < bytes.len() {
            if bytes[index] == close && bytes[index + 1] == b'\'' {
                return index + 2;
            }
            index += 1;
        }
        return bytes.len();
    }
    // `''` inside a literal reads as two adjacent literals, which ends the same place.
    sql[at + 1..]
        .find('\'')
        .map_or(bytes.len(), |offset| at + 1 + offset + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_terminator_from_plain_statements() {
        assert_eq!(executable_sql("select 1 from dual;"), "select 1 from dual");
        assert_eq!(
            executable_sql("select 1 from dual ;\n"),
            "select 1 from dual"
        );
        assert_eq!(executable_sql("select 1 from dual;;"), "select 1 from dual");
        assert_eq!(executable_sql("commit\n/\n"), "commit");
        assert_eq!(
            executable_sql("select 1 from dual; -- done\n/* really */"),
            "select 1 from dual"
        );
    }

    #[test]
    fn keeps_the_terminator_plsql_needs() {
        assert_eq!(executable_sql("begin null; end;"), "begin null; end;");
        assert_eq!(executable_sql("begin null; end;\n/\n"), "begin null; end;");
        assert_eq!(
            executable_sql("declare x number; begin x := 1; end; -- ok"),
            "declare x number; begin x := 1; end;"
        );
        assert_eq!(
            executable_sql("CREATE OR REPLACE EDITIONABLE PROCEDURE p AS BEGIN NULL; END;\n/"),
            "CREATE OR REPLACE EDITIONABLE PROCEDURE p AS BEGIN NULL; END;"
        );
        assert_eq!(
            executable_sql("create trigger t before insert on x begin null; end;"),
            "create trigger t before insert on x begin null; end;"
        );
        assert_eq!(
            executable_sql("create table t (a number);"),
            "create table t (a number)"
        );
    }

    #[test]
    fn strips_leading_comments() {
        assert_eq!(
            executable_sql("-- who\n/* what */\n  select 1 from dual"),
            "select 1 from dual"
        );
    }

    #[test]
    fn does_not_mistake_quoted_text_for_structure() {
        assert_eq!(
            executable_sql("select ';' from dual"),
            "select ';' from dual"
        );
        assert_eq!(
            executable_sql("select '--' from dual;"),
            "select '--' from dual"
        );
        assert_eq!(
            executable_sql("select 'it''s;' from dual;"),
            "select 'it''s;' from dual"
        );
        assert_eq!(
            executable_sql("select q'[it's; --]' from dual;"),
            "select q'[it's; --]' from dual"
        );
        assert_eq!(
            executable_sql("select nq'{a'b}' \"x;y\" from dual;"),
            "select nq'{a'b}' \"x;y\" from dual"
        );
        assert_eq!(
            executable_sql("select 4 / 2 from dual"),
            "select 4 / 2 from dual"
        );
    }

    #[test]
    fn a_comment_only_statement_is_empty() {
        assert_eq!(executable_sql("-- nothing"), "");
        assert_eq!(executable_sql(" ; "), "");
        assert_eq!(executable_sql("/"), "/");
    }

    #[test]
    fn classifies_what_a_statement_does_to_the_transaction() {
        use TransactionEffect::*;
        assert_eq!(transaction_effect("commit"), Closes);
        assert_eq!(transaction_effect("-- x\nCOMMIT WORK"), Closes);
        assert_eq!(transaction_effect("rollback"), Closes);
        assert_eq!(transaction_effect("rollback work"), Closes);
        assert_eq!(transaction_effect("rollback to s1"), None);
        assert_eq!(transaction_effect("ROLLBACK WORK TO SAVEPOINT s1"), None);
        assert_eq!(transaction_effect("savepoint s1"), Opens);
        assert_eq!(transaction_effect("set transaction read only"), Opens);
        assert_eq!(transaction_effect("set role all"), None);
        assert_eq!(transaction_effect("update t set a = 1"), None);
        assert_eq!(transaction_effect("committed"), None);
    }

    #[test]
    fn recognizes_explain_plan() {
        assert!(is_explain_plan("explain plan for select 1 from dual"));
        assert!(is_explain_plan(
            "/* x */ EXPLAIN  PLAN\nFOR select 1 from dual"
        ));
        assert!(!is_explain_plan("select 'explain plan for' from dual"));
    }
}

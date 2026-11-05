//! The spellings of an Oracle connection string people paste.
//!
//! `oracle-rs` reads EZConnect only: `host[:port][/service]` or `host:port:sid`.
//! The form advertises `oracle://user:password@host:1521/service`, and SQL
//! Developer and DBeaver export JDBC URLs, so turn each of those into EZConnect
//! and hand any credentials they carried back to the caller.

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConnectString {
    pub user: Option<String>,
    pub password: Option<String>,
    /// EZConnect form, ready for `Config::from_str`.
    pub target: String,
}

/// Parse a connection string. Query parameters are left to `parse_wallet_params`.
pub fn parse_connect_string(url: &str) -> ConnectString {
    let text = url.trim();
    let text = text.split('?').next().unwrap_or(text);

    let (rest, url_encoded) = if let Some(rest) = strip_prefix_ignore_case(text, "oracle://") {
        (rest, true)
    } else if let Some(rest) = strip_prefix_ignore_case(text, "jdbc:oracle:thin:") {
        (rest, false)
    } else {
        (text, false)
    };

    // The last `@` ends the credentials: a raw password may hold one, a host cannot.
    let (credentials, target) = match rest.rsplit_once('@') {
        Some((credentials, target)) => (credentials, target),
        None => ("", rest),
    };
    let (user, password) = if url_encoded {
        let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
        (percent_decode(user, false), percent_decode(password, false))
    } else {
        let (user, password) = credentials.split_once('/').unwrap_or((credentials, ""));
        (user.to_string(), password.to_string())
    };

    ConnectString {
        user: non_empty(user),
        password: non_empty(password),
        target: normalize_target(target.trim_start_matches('/')),
    }
}

/// `host:port/sid:NAME` means a SID, as it does in the form's service field.
fn normalize_target(target: &str) -> String {
    let Some((address, service)) = target.split_once('/') else {
        return target.to_string();
    };
    if let Some(sid) = service.strip_prefix("sid:") {
        let address = if address.contains(':') {
            address.to_string()
        } else {
            format!("{address}:1521")
        };
        return format!("{address}:{sid}");
    }
    let service = service.strip_prefix("service:").unwrap_or(service);
    format!("{address}/{service}")
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// Decode `%XX` escapes as bytes, so multi-byte UTF-8 survives. `+` is a space
/// in a query string and a literal plus everywhere else.
pub fn percent_decode(value: &str, plus_is_space: bool) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (b'+', _) if plus_is_space => {
                out.push(b' ');
                index += 1;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(user: Option<&str>, password: Option<&str>, target: &str) -> ConnectString {
        ConnectString {
            user: user.map(str::to_string),
            password: password.map(str::to_string),
            target: target.to_string(),
        }
    }

    #[test]
    fn reads_the_url_the_form_advertises() {
        assert_eq!(
            parse_connect_string("oracle://scott:tiger@db:1521/FREEPDB1"),
            parsed(Some("scott"), Some("tiger"), "db:1521/FREEPDB1")
        );
        assert_eq!(
            parse_connect_string("ORACLE://db:1521/FREEPDB1"),
            parsed(None, None, "db:1521/FREEPDB1")
        );
    }

    #[test]
    fn decodes_url_credentials_and_keeps_raw_ones_verbatim() {
        assert_eq!(
            parse_connect_string("oracle://sc%40tt:p%2Fw%E3%81%82+1@db/svc"),
            parsed(Some("sc@tt"), Some("p/wあ+1"), "db/svc")
        );
        assert_eq!(
            parse_connect_string("scott/p%40w@ss@db:1521/svc"),
            parsed(Some("scott"), Some("p%40w@ss"), "db:1521/svc")
        );
    }

    #[test]
    fn reads_jdbc_urls() {
        assert_eq!(
            parse_connect_string("jdbc:oracle:thin:@//db:1521/svc"),
            parsed(None, None, "db:1521/svc")
        );
        assert_eq!(
            parse_connect_string("jdbc:oracle:thin:scott/tiger@//db:1521/svc"),
            parsed(Some("scott"), Some("tiger"), "db:1521/svc")
        );
        assert_eq!(
            parse_connect_string("jdbc:oracle:thin:@db:1521:ORCL"),
            parsed(None, None, "db:1521:ORCL")
        );
    }

    #[test]
    fn reads_ezconnect_and_drops_query_parameters() {
        assert_eq!(
            parse_connect_string("//db:1521/svc?wallet=%2Ftmp%2Fw"),
            parsed(None, None, "db:1521/svc")
        );
        assert_eq!(parse_connect_string("db"), parsed(None, None, "db"));
    }

    #[test]
    fn reads_the_service_prefixes_the_form_accepts() {
        assert_eq!(
            parse_connect_string("oracle://u:p@db:1522/sid:ORCL").target,
            "db:1522:ORCL"
        );
        assert_eq!(
            parse_connect_string("oracle://u:p@db/sid:ORCL").target,
            "db:1521:ORCL"
        );
        assert_eq!(
            parse_connect_string("oracle://u:p@db:1521/service:svc").target,
            "db:1521/svc"
        );
    }

    #[test]
    fn percent_decode_leaves_a_stray_percent_alone() {
        assert_eq!(percent_decode("100%", false), "100%");
        assert_eq!(percent_decode("%zz%4", false), "%zz%4");
        assert_eq!(percent_decode("a+b", true), "a b");
        assert_eq!(percent_decode("a+b", false), "a+b");
    }
}

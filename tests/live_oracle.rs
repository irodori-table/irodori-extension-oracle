//! Drives the connector through its ABI against a real server, with the exact
//! request shape the desktop host sends. Skips unless IRODORI_ORACLE is set:
//!
//!     docker run -d -p 55521:1521 -e ORACLE_PASSWORD=irodori \
//!       -e APP_USER=irodori -e APP_USER_PASSWORD=irodori \
//!       gvenzl/oracle-free:slim-faststart
//!     IRODORI_ORACLE=1 cargo test --test live_oracle -- --test-threads=1

use irodori_extension_oracle::{
    irodori_connector_call_json, irodori_connector_free_buffer, IrodoriConnectorBuffer,
};
use serde_json::{json, Value};

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn host() -> String {
    env("IRODORI_ORACLE_HOST", "127.0.0.1")
}

fn port() -> u16 {
    env("IRODORI_ORACLE_PORT", "55521").parse().unwrap()
}

fn user() -> String {
    env("IRODORI_ORACLE_USER", "irodori")
}

fn password() -> String {
    env("IRODORI_ORACLE_PASSWORD", "irodori")
}

fn service() -> String {
    env("IRODORI_ORACLE_SERVICE", "FREEPDB1")
}

fn enabled() -> bool {
    let on = std::env::var("IRODORI_ORACLE").is_ok();
    if !on {
        eprintln!("skip: IRODORI_ORACLE not set");
    }
    on
}

fn call(request: Value) -> Value {
    let text = request.to_string();
    let reply = irodori_connector_call_json(IrodoriConnectorBuffer {
        ptr: text.as_ptr(),
        len: text.len(),
    });
    let bytes = unsafe { std::slice::from_raw_parts(reply.ptr, reply.len) };
    let value = serde_json::from_slice(bytes).unwrap();
    irodori_connector_free_buffer(reply);
    value
}

/// `connect_request` in the host's extensions/connection.rs: the profile's
/// fields at the top level, and again under `profile`.
fn host_request(id: &str, profile: Value) -> Value {
    let mut request = profile.clone();
    let object = request.as_object_mut().unwrap();
    object.insert("method".into(), json!("connect"));
    object.insert("connectionId".into(), json!(id));
    object.insert("profile".into(), profile);
    request
}

fn fields_profile(id: &str, database: Value, user_value: Value, password_value: Value) -> Value {
    json!({
        "id": id,
        "engine": "oracle",
        "host": host(),
        "port": port(),
        "user": user_value,
        "password": password_value,
        "database": database,
        "readOnly": false,
        "options": { "endpointMode": "hostPort", "authMethod": "userPassword", "tlsMode": "disable" },
    })
}

fn url_profile(id: &str, url: String) -> Value {
    json!({
        "id": id,
        "engine": "oracle",
        "url": url,
        "readOnly": false,
        "options": { "endpointMode": "connectionString", "authMethod": "connectionString", "tlsMode": "disable" },
    })
}

fn close(id: &str) {
    call(json!({ "method": "close", "connectionId": id }));
}

fn assert_connects(id: &str, profile: Value) {
    let reply = call(host_request(id, profile));
    assert_eq!(reply["ok"], true, "{id}: {reply}");
    let rows = call(json!({
        "method": "query",
        "connectionId": id,
        "sql": "select sys_context('USERENV','SESSION_USER') from dual",
        "maxRows": 1,
    }));
    assert_eq!(rows["ok"], true, "{id}: {rows}");
    assert_eq!(
        rows["rows"][0][0],
        json!(user().to_uppercase()),
        "{id}: {rows}"
    );
    close(id);
}

#[test]
fn connects_with_the_fields_the_form_collects() {
    if !enabled() {
        return;
    }
    assert_connects(
        "fields",
        fields_profile("fields", json!(service()), json!(user()), json!(password())),
    );
}

#[test]
fn connects_with_an_explicit_service_prefix() {
    if !enabled() {
        return;
    }
    assert_connects(
        "service-prefix",
        fields_profile(
            "service-prefix",
            json!(format!("service:{}", service())),
            json!(user()),
            json!(password()),
        ),
    );
}

#[test]
fn connects_with_each_connection_string_spelling() {
    if !enabled() {
        return;
    }
    let (u, p, h, port, s) = (user(), password(), host(), port(), service());
    for (id, url) in [
        ("oracle-url", format!("oracle://{u}:{p}@{h}:{port}/{s}")),
        (
            "jdbc-creds",
            format!("jdbc:oracle:thin:{u}/{p}@//{h}:{port}/{s}"),
        ),
        ("easy-creds", format!("{u}/{p}@{h}:{port}/{s}")),
    ] {
        assert_connects(id, url_profile(id, url));
    }
}

#[test]
fn a_connection_string_takes_credentials_from_the_profile_too() {
    if !enabled() {
        return;
    }
    let (u, p, h, port, s) = (user(), password(), host(), port(), service());
    for (id, url) in [
        ("jdbc-bare", format!("jdbc:oracle:thin:@//{h}:{port}/{s}")),
        ("easy-bare", format!("//{h}:{port}/{s}")),
        ("oracle-bare", format!("oracle://{h}:{port}/{s}")),
    ] {
        let mut profile = url_profile(id, url);
        profile["user"] = json!(u);
        profile["password"] = json!(p);
        assert_connects(id, profile);
    }
}

#[test]
fn reports_what_was_wrong_with_a_failed_connection() {
    if !enabled() {
        return;
    }
    let cases = [
        (
            "no-user",
            json!(service()),
            Value::Null,
            Value::Null,
            "needs a user name",
        ),
        (
            "bad-password",
            json!(service()),
            json!(user()),
            json!("wrong"),
            "ORA-01017",
        ),
        (
            "bad-service",
            json!("NOPE"),
            json!(user()),
            json!(password()),
            "ORA-12514",
        ),
        (
            "bad-sid",
            json!("sid:NOPE"),
            json!(user()),
            json!(password()),
            "ORA-12505",
        ),
    ];
    for (id, database, user_value, password_value, expected) in cases {
        let reply = call(host_request(
            id,
            fields_profile(id, database, user_value, password_value),
        ));
        assert_eq!(reply["ok"], false, "{id}: {reply}");
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains(expected), "{id}: {message}");
        assert!(!message.contains("wrong"), "{id} leaked: {message}");
    }
}

/// A connection for one test, closed when it drops.
struct Session(&'static str);

impl Session {
    fn open(id: &'static str) -> Self {
        let reply = call(host_request(
            id,
            fields_profile(id, json!(service()), json!(user()), json!(password())),
        ));
        assert_eq!(reply["ok"], true, "{id}: {reply}");
        Session(id)
    }

    fn try_run(&self, sql: &str, cap: usize) -> Value {
        call(json!({ "method": "query", "connectionId": self.0, "sql": sql, "maxRows": cap }))
    }

    fn run(&self, sql: &str) -> Value {
        let reply = self.try_run(sql, 100_000);
        assert_eq!(reply["ok"], true, "{sql}: {reply}");
        reply
    }

    fn error(&self, sql: &str) -> String {
        let reply = self.try_run(sql, 100);
        assert_eq!(reply["ok"], false, "{sql}: {reply}");
        reply["error"]["message"].as_str().unwrap().to_string()
    }

    fn drop_table(&self, name: &str) {
        self.try_run(&format!("drop table {name} purge"), 1);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        close(self.0);
    }
}

#[test]
fn a_failed_statement_reports_its_error_and_keeps_the_connection() {
    if !enabled() {
        return;
    }
    let db = Session::open("errors");
    let message = db.error("select * from no_such_table");
    assert!(message.contains("ORA-00942"), "{message}");
    assert!(message.contains("does not exist"), "{message}");
    assert_eq!(
        message.matches("ORA-00942").count(),
        1,
        "code twice: {message}"
    );
    assert_eq!(db.run("select 1 from dual")["rows"], json!([[1]]));

    let message = db.error("selec bad");
    assert!(message.contains("ORA-00900"), "{message}");
    assert_eq!(db.run("select 2 from dual")["rows"], json!([[2]]));

    db.drop_table("irodori_dup");
    db.run("create table irodori_dup (id number primary key)");
    db.run("insert into irodori_dup values (1)");
    let message = db.error("insert into irodori_dup values (1)");
    assert!(message.contains("ORA-00001"), "{message}");
    assert_eq!(
        db.run("select count(*) from irodori_dup")["rows"],
        json!([[1]])
    );
    db.drop_table("irodori_dup");
}

#[test]
fn runs_ddl_dml_and_commits_what_they_did() {
    if !enabled() {
        return;
    }
    let db = Session::open("dml");
    db.drop_table("irodori_live");
    assert_eq!(
        db.run("create table irodori_live (id number(10) primary key, label varchar2(40))")
            ["columns"],
        json!([])
    );
    let inserted = db.run("insert into irodori_live values (1, 'サービス名')");
    assert_eq!(inserted["columns"], json!(["ROWS_AFFECTED"]));
    assert_eq!(inserted["rows"], json!([[1]]));
    db.run("insert into irodori_live values (2, 'b')");
    assert_eq!(
        db.run("update irodori_live set label = 'c'")["rows"],
        json!([[2]])
    );
    assert_eq!(
        db.run("delete from irodori_live where id = 2")["rows"],
        json!([[1]])
    );
    drop(db);

    // A new session sees it: nothing waited on a COMMIT.
    let db = Session::open("dml-2");
    let rows = db.run("select id, label from irodori_live order by id");
    assert_eq!(rows["columns"], json!(["ID", "LABEL"]));
    assert_eq!(rows["rows"], json!([[1, "c"]]));
    db.drop_table("irodori_live");
}

#[test]
fn a_savepoint_or_set_transaction_hands_the_transaction_to_the_user() {
    if !enabled() {
        return;
    }
    let db = Session::open("manual");
    db.drop_table("irodori_tx");
    db.run("create table irodori_tx (id number)");

    // ROLLBACK undoes what ran since SET TRANSACTION.
    db.run("set transaction name 'by_hand'");
    db.run("insert into irodori_tx values (1)");
    db.run("rollback");
    assert_eq!(
        db.run("select count(*) from irodori_tx")["rows"],
        json!([[0]])
    );

    // ROLLBACK TO stays inside it, COMMIT ends it.
    db.run("savepoint s1");
    db.run("insert into irodori_tx values (2)");
    db.run("savepoint s2");
    db.run("insert into irodori_tx values (3)");
    db.run("rollback to s2");
    assert_eq!(db.run("select id from irodori_tx")["rows"], json!([[2]]));
    db.run("commit");

    // Back to committing each statement.
    db.run("insert into irodori_tx values (4)");
    db.run("rollback");
    assert_eq!(
        db.run("select id from irodori_tx order by id")["rows"],
        json!([[2], [4]])
    );
    db.drop_table("irodori_tx");
}

#[test]
fn outlives_the_open_cursor_limit() {
    if !enabled() {
        return;
    }
    let db = Session::open("cursors");
    db.drop_table("irodori_many");
    db.run("create table irodori_many (id number)");
    // OPEN_CURSORS defaults to 300.
    for index in 0..500 {
        db.run(&format!("select {index} from dual"));
        db.run(&format!("insert into irodori_many values ({index})"));
        db.error("select * from no_such_table");
    }
    // Every other kind of statement, and every way of failing, too.
    for index in 0..350 {
        db.run("begin null; end;");
        db.run("commit");
        db.run("explain plan for select 1 from dual");
        db.run(&format!("create table irodori_cursor_{index} (id number)"));
        db.run(&format!("drop table irodori_cursor_{index} purge"));
        db.error("begin no_such_procedure; end;");
        db.error("drop table no_such_table");
        db.error("insert into no_such_table values (1)");
    }
    // A statement left unread gives its cursor back too.
    for index in 0..350 {
        let reply = db.try_run(
            &format!("select level + {index} from dual connect by level <= 1000"),
            5,
        );
        assert_eq!(reply["truncated"], true, "{reply}");
    }
    assert_eq!(
        db.run("select count(*) from irodori_many")["rows"],
        json!([[500]])
    );
    db.drop_table("irodori_many");
}

#[test]
fn reads_every_row_of_a_large_result() {
    if !enabled() {
        return;
    }
    let db = Session::open("rows");
    for count in [1, 99, 100, 101, 356, 357, 5000] {
        let reply = db.run(&format!(
            "select level, 'same' from dual connect by level <= {count}"
        ));
        let rows = reply["rows"].as_array().unwrap();
        assert_eq!(rows.len(), count, "{count}");
        assert_eq!(reply["truncated"], false, "{count}");
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(*row, json!([index + 1, "same"]), "{count}");
        }
    }
    let empty = db.run("select 1 from dual where 1 = 0");
    assert_eq!(empty["columns"], json!(["1"]));
    assert_eq!(empty["rows"], json!([]));
}

#[test]
fn reports_truncation_only_when_rows_were_left_behind() {
    if !enabled() {
        return;
    }
    let db = Session::open("cap");
    let sql = "select level from dual connect by level <= 1000";
    for (cap, rows, truncated) in [
        (50, 50, true),
        (100, 100, true),
        (101, 101, true),
        (999, 999, true),
        (1000, 1000, false),
        (5000, 1000, false),
    ] {
        let reply = db.try_run(sql, cap);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["rows"].as_array().unwrap().len(), rows, "cap {cap}");
        assert_eq!(reply["truncated"], truncated, "cap {cap}");
    }
    assert_eq!(db.run("select 'next' from dual")["rows"], json!([["next"]]));
}

#[test]
fn reads_rows_that_span_many_packets() {
    if !enabled() {
        return;
    }
    let db = Session::open("wide");
    let wide = (0..8)
        .map(|column| format!("rpad('{column}', 3900, 'x')"))
        .collect::<Vec<_>>()
        .join(", ");
    for count in [10, 90, 600] {
        let reply = db.run(&format!(
            "select level, {wide} from dual connect by level <= {count}"
        ));
        let rows = reply["rows"].as_array().unwrap();
        assert_eq!(rows.len(), count);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row[0], json!(index + 1));
            for column in 0..8 {
                let cell = row[column + 1].as_str().unwrap();
                assert_eq!(cell.len(), 3900);
                assert!(cell.starts_with(&column.to_string()));
            }
        }
    }
    assert_eq!(
        db.run("select 'alive' from dual")["rows"],
        json!([["alive"]])
    );
}

#[test]
fn shows_each_data_type_as_its_value() {
    if !enabled() {
        return;
    }
    let db = Session::open("types");
    db.drop_table("irodori_types");
    db.run(
        "create table irodori_types (
           n number, big number, frac number(10,2), f binary_float, d binary_double,
           s varchar2(20), c char(3), ns nvarchar2(20), r raw(2),
           dt date, ts timestamp(3), tz timestamp(3) with time zone,
           cl clob, bl blob, empty_cl clob, nothing varchar2(1),
           ds interval day(2) to second(3), ym interval year(2) to month
         )",
    );
    db.run(
        "insert into irodori_types values (
           42, 12345678901234567890, 1.5, 0.5, 2.25,
           'text', 'ab', N'サービス', hextoraw('DEAD'),
           date '2026-11-05', timestamp '2026-11-05 01:02:03.250',
           timestamp '2026-11-05 01:02:03.250 +09:00',
           to_clob('クロブ本文'), to_blob(hextoraw('BEEF')), empty_clob(), null,
           interval '1 02:03:04.500' day to second, interval '3-4' year to month
         )",
    );
    let reply = db.run("select * from irodori_types");
    let row = &reply["rows"][0];
    let column = |name: &str| {
        let names = reply["columns"].as_array().unwrap();
        let index = names.iter().position(|item| item == name).unwrap();
        row[index].clone()
    };
    assert_eq!(column("N"), json!(42));
    assert_eq!(column("BIG"), json!("12345678901234567890"));
    assert_eq!(column("FRAC"), json!(1.5));
    assert_eq!(column("F"), json!(0.5));
    assert_eq!(column("D"), json!(2.25));
    assert_eq!(column("S"), json!("text"));
    assert_eq!(column("C"), json!("ab "));
    assert_eq!(column("NS"), json!("サービス"));
    assert_eq!(column("R"), json!("\\xdead"));
    assert_eq!(column("DT"), json!("2026-11-05 00:00:00"));
    assert_eq!(column("TS"), json!("2026-11-05 01:02:03.250000"));
    assert_eq!(column("TZ"), json!("2026-11-05 01:02:03.250000 +09:00"));
    assert_eq!(column("CL"), json!("クロブ本文"));
    assert_eq!(column("BL"), json!("\\xbeef"));
    assert_eq!(column("EMPTY_CL"), json!(""));
    assert_eq!(column("NOTHING"), Value::Null);
    assert_eq!(column("DS"), json!("+01 02:03:04.500000"));
    assert_eq!(column("YM"), json!("+03-04"));

    let rowid = db.run("select rowid from irodori_types");
    let rowid = rowid["rows"][0][0].as_str().unwrap().to_string();
    assert_eq!(rowid.len(), 18, "{rowid}");
    assert!(
        rowid
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '/'),
        "{rowid}"
    );
    assert_eq!(
        db.run(&format!(
            "select n from irodori_types where rowid = '{rowid}'"
        ))["rows"],
        json!([[42]])
    );
    db.drop_table("irodori_types");
}

#[test]
fn reads_a_result_with_lob_columns_past_one_batch() {
    if !enabled() {
        return;
    }
    let db = Session::open("lobs");
    let reply = db.run("select level, to_clob('row ' || level) from dual connect by level <= 450");
    let rows = reply["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 450);
    assert_eq!(rows[449], json!([450, "row 450"]));
}

#[test]
fn reads_long_columns_and_stays_in_step() {
    if !enabled() {
        return;
    }
    let db = Session::open("long");
    db.drop_table("irodori_long");
    db.run("create table irodori_long (id number, body long)");
    db.run("insert into irodori_long values (1, 'short')");
    db.run("insert into irodori_long values (2, null)");
    let long_text = "x".repeat(30_000);
    db.run(&format!(
        "declare v long := '{}';\nbegin\n  v := v || v || v;\n  insert into irodori_long values (3, v);\nend;",
        "x".repeat(10_000)
    ));
    let reply = db.run("select id, body from irodori_long order by id");
    assert_eq!(reply["rows"][0], json!([1, "short"]));
    assert_eq!(reply["rows"][1], json!([2, null]));
    assert_eq!(reply["rows"][2], json!([3, long_text]));
    assert_eq!(db.run("select 'next' from dual")["rows"], json!([["next"]]));

    // Dictionary views are where people meet LONG.
    db.run("create or replace view irodori_view as select 1 as a from dual");
    let views = db.run("select view_name, text from user_views where view_name = 'IRODORI_VIEW'");
    assert!(
        views["rows"][0][1].as_str().unwrap().contains("select 1"),
        "{views}"
    );
    db.run("drop view irodori_view");
    db.drop_table("irodori_long");
}

#[test]
fn runs_a_statement_larger_than_the_sdu() {
    if !enabled() {
        return;
    }
    let db = Session::open("big-sql");
    for size in [8_000, 8_200, 70_000, 300_000] {
        let filler = "x".repeat(size);
        let reply = db.run(&format!("select /* {filler} */ {size} from dual"));
        assert_eq!(reply["rows"], json!([[size]]));
    }
    db.drop_table("irodori_big");
    db.run("create table irodori_big (id number)");
    let filler = "x".repeat(40_000);
    db.run(&format!(
        "begin /* {filler} */ insert into irodori_big values (7); end;"
    ));
    assert_eq!(db.run("select id from irodori_big")["rows"], json!([[7]]));
    db.drop_table("irodori_big");
}

#[test]
fn shows_a_named_time_zone_as_its_instant() {
    if !enabled() {
        return;
    }
    let db = Session::open("tz");
    let reply = db.run("select timestamp '2026-11-05 01:02:03 Asia/Tokyo' from dual");
    assert_eq!(reply["rows"], json!([["2026-11-04 16:02:03.000000 UTC"]]));
    assert_eq!(db.run("select 'next' from dual")["rows"], json!([["next"]]));
}

#[test]
fn accepts_sql_the_way_an_editor_sends_it() {
    if !enabled() {
        return;
    }
    let db = Session::open("text");
    for sql in [
        "select 1 from dual;",
        "select 1 from dual ;\n",
        "-- leading comment\nselect 1 from dual",
        "/* block */ select 1 from dual; -- trailing",
        "select 1 from dual\n/\n",
        "select*from (select 1 from dual)",
        "(select 1 from dual)",
        "with t as (select 1 from dual) select * from t",
    ] {
        assert_eq!(db.run(sql)["rows"], json!([[1]]), "{sql}");
    }
    assert_eq!(db.run("select ';' from dual;")["rows"], json!([[";"]]));
}

#[test]
fn runs_plsql_and_keeps_the_terminator_it_needs() {
    if !enabled() {
        return;
    }
    let db = Session::open("plsql");
    db.drop_table("irodori_pl");
    db.run("create table irodori_pl (id number)");
    db.run("begin insert into irodori_pl values (1); end;");
    db.run("declare v number := 2;\nbegin\n  insert into irodori_pl values (v);\nend;\n/\n");
    assert_eq!(
        db.run("select id from irodori_pl order by id")["rows"],
        json!([[1], [2]])
    );
    db.run(
        "create or replace procedure irodori_proc as begin insert into irodori_pl values (3); end;",
    );
    db.run("begin irodori_proc; end;");
    assert_eq!(
        db.run("select count(*) from irodori_pl")["rows"],
        json!([[3]])
    );
    db.run("drop procedure irodori_proc");
    db.drop_table("irodori_pl");
}

#[test]
fn explains_a_plan() {
    if !enabled() {
        return;
    }
    let db = Session::open("plan");
    let reply = db.run("explain plan for select * from dual");
    assert_eq!(reply["columns"], json!(["PLAN_TABLE_OUTPUT"]));
    assert!(reply["rows"].to_string().contains("DUAL"), "{reply}");
}

#[test]
fn describes_a_schema_of_any_size() {
    if !enabled() {
        return;
    }
    let db = Session::open("schema");
    let names = (0..130)
        .map(|i| format!("irodori_meta_{i}"))
        .collect::<Vec<_>>();
    for name in &names {
        db.drop_table(name);
        db.run(&format!(
            "create table {name} (id number primary key, label varchar2(10))"
        ));
    }
    let reply = call(json!({ "method": "metadata", "connectionId": "schema" }));
    assert_eq!(reply["ok"], true, "{reply}");
    let objects = reply["metadata"]["schemas"][0]["objects"]
        .as_array()
        .unwrap();
    for name in &names {
        let wanted = name.to_uppercase();
        let object = objects
            .iter()
            .find(|object| object["name"] == wanted)
            .unwrap_or_else(|| panic!("{wanted} missing"));
        assert_eq!(object["kind"], "table");
        assert_eq!(object["columns"].as_array().unwrap().len(), 2);
        assert_eq!(object["primaryKey"], json!(["ID"]));
    }
    for name in &names {
        db.drop_table(name);
    }
}

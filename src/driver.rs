use irodori_connector_abi::{option_string, push_sensitive, request_containers};
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use oracle_rs::constants::OracleType;
use oracle_rs::row::Row;
use oracle_rs::statement::ColumnInfo;
use oracle_rs::types::{LobData, LobLocator, LobValue};
use oracle_rs::{Config, Connection as OraConn, Error as OraError, Value as OraValue};
use serde_json::{json, Map, Value};
use tokio::runtime::Runtime;
use tokio::sync::Mutex as AsyncMutex;

use crate::abi::{self, IrodoriConnectorBuffer};
use crate::connect_string::{parse_connect_string, percent_decode};
use crate::statement_text::{
    executable_sql, is_explain_plan, transaction_effect, TransactionEffect,
};
use crate::{ABI_VERSION, CONFIG_JSON, DRIVER_LINKED, ENGINE, MANIFEST_JSON};

/// Rows asked for per round trip after the first batch.
const FETCH_BATCH: u32 = 256;
/// The most of a LOB one cell shows: characters for CLOB, bytes for BLOB.
const MAX_LOB_CELL: u64 = 1 << 20;
/// Integers beyond this lose digits in the UI's JSON numbers.
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

static CONNECTIONS: OnceLock<Mutex<HashMap<String, OracleConnection>>> = OnceLock::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[derive(Clone)]
struct OracleConnection {
    conn: Arc<AsyncMutex<OraConn>>,
    config: OracleConfig,
    /// The user opened a transaction by hand, so statements stop committing
    /// themselves until they close it.
    manual_transaction: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OracleConfig {
    target: String,
    username: String,
    redaction_values: Vec<String>,
}

#[derive(Default)]
struct ObjectMeta {
    kind: String,
    columns: Vec<Value>,
    indexes: Vec<Value>,
    primary_key: Vec<String>,
    foreign_keys: Vec<Value>,
}

type QueryRows = Vec<Vec<Value>>;
type QueryOutput = (Vec<String>, QueryRows, bool);

fn connections() -> &'static Mutex<HashMap<String, OracleConnection>> {
    CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn runtime() -> Result<&'static Runtime, String> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = Runtime::new().map_err(|err| format!("create tokio runtime failed: {err}"))?;
    let _ = RUNTIME.set(runtime);
    RUNTIME
        .get()
        .ok_or_else(|| "create tokio runtime failed.".to_string())
}

pub fn call_json(request: IrodoriConnectorBuffer) -> IrodoriConnectorBuffer {
    let request = match abi::parse_request(request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let method = match abi::request_method(request.as_ref()) {
        Ok(method) => method,
        Err(response) => return response,
    };

    match method {
        "health" | "ping" => abi::ok(Map::from_iter([
            ("engine".to_string(), Value::String(ENGINE.to_string())),
            ("abiVersion".to_string(), json!(ABI_VERSION)),
            ("driverLinked".to_string(), Value::Bool(DRIVER_LINKED)),
        ])),
        "describe" | "capabilities" => abi::ok(Map::from_iter([
            ("engine".to_string(), Value::String(ENGINE.to_string())),
            ("abiVersion".to_string(), json!(ABI_VERSION)),
            ("driverLinked".to_string(), Value::Bool(DRIVER_LINKED)),
            (
                "manifest".to_string(),
                serde_json::from_str(MANIFEST_JSON).unwrap_or(Value::Null),
            ),
            (
                "config".to_string(),
                serde_json::from_str(CONFIG_JSON).unwrap_or(Value::Null),
            ),
        ])),
        "manifest" => abi::owned_buffer(MANIFEST_JSON.to_string()),
        "config" => abi::owned_buffer(CONFIG_JSON.to_string()),
        "connect" => connect(request.as_ref().expect("connect has request")),
        "query" => query(request.as_ref().expect("query has request")),
        "metadata" => metadata(request.as_ref().expect("metadata has request")),
        "close" => close(request.as_ref().expect("close has request")),
        other => abi::error(
            "connector.unknownMethod",
            format!("unknown connector method: {other}"),
        ),
    }
}

fn connect(request: &Value) -> IrodoriConnectorBuffer {
    let connection_id = abi::connection_id(Some(request));
    let (driver_config, connector_config) = match OracleConfig::from_request(request) {
        Ok(config) => config,
        Err(err) => return abi::error("connector.invalidRequest", err),
    };
    let conn = match runtime().and_then(|runtime| {
        runtime
            .block_on(OraConn::connect_with_config(driver_config))
            .map_err(|err| describe_error(&err))
    }) {
        Ok(conn) => conn,
        Err(err) => return abi::error("connector.connectFailed", connector_config.redact(&err)),
    };
    let connection = OracleConnection {
        conn: Arc::new(AsyncMutex::new(conn)),
        config: connector_config,
        manual_transaction: Arc::new(AtomicBool::new(false)),
    };
    let server_version = runtime()
        .and_then(|runtime| runtime.block_on(version(&connection)))
        .unwrap_or_else(|_| "Oracle".to_string());
    let mut guard = match connections().lock() {
        Ok(guard) => guard,
        Err(_) => {
            return abi::error(
                "connector.statePoisoned",
                "Connector connection state is poisoned.",
            )
        }
    };
    let response = Map::from_iter([
        ("engine".to_string(), Value::String(ENGINE.to_string())),
        (
            "connectionId".to_string(),
            Value::String(connection_id.clone()),
        ),
        ("driverLinked".to_string(), Value::Bool(DRIVER_LINKED)),
        (
            "database".to_string(),
            Value::String(connection.config.target.clone()),
        ),
        (
            "user".to_string(),
            Value::String(connection.config.username.clone()),
        ),
        ("serverVersion".to_string(), Value::String(server_version)),
    ]);
    guard.insert(connection_id, connection);
    abi::ok(response)
}

fn query(request: &Value) -> IrodoriConnectorBuffer {
    let connection_id = abi::connection_id(Some(request));
    let Some(sql) = abi::string_field(request, "sql")
        .or_else(|| abi::string_field(request, "query"))
        .or_else(|| abi::string_field(request, "statement"))
    else {
        return abi::error(
            "connector.invalidRequest",
            "query requires a string sql, query, or statement field.",
        );
    };
    let connection = match connection(&connection_id) {
        Ok(connection) => connection,
        Err(response) => return response,
    };
    match runtime()
        .and_then(|runtime| runtime.block_on(run_query(&connection, sql, abi::max_rows(request))))
    {
        Ok((columns, rows, truncated)) => abi::ok(Map::from_iter([
            ("connectionId".to_string(), Value::String(connection_id)),
            (
                "columns".to_string(),
                Value::Array(columns.into_iter().map(Value::String).collect()),
            ),
            (
                "rows".to_string(),
                Value::Array(rows.into_iter().map(Value::Array).collect()),
            ),
            ("truncated".to_string(), Value::Bool(truncated)),
        ])),
        Err(err) => abi::error("connector.queryFailed", connection.config.redact(&err)),
    }
}

fn metadata(request: &Value) -> IrodoriConnectorBuffer {
    let connection_id = abi::connection_id(Some(request));
    let connection = match connection(&connection_id) {
        Ok(connection) => connection,
        Err(response) => return response,
    };
    match runtime().and_then(|runtime| runtime.block_on(load_metadata(&connection))) {
        Ok(metadata) => abi::ok(Map::from_iter([
            ("connectionId".to_string(), Value::String(connection_id)),
            ("metadata".to_string(), metadata),
        ])),
        Err(err) => abi::error("connector.metadataFailed", connection.config.redact(&err)),
    }
}

fn close(request: &Value) -> IrodoriConnectorBuffer {
    let connection_id = abi::connection_id(Some(request));
    let closed = match connections().lock() {
        Ok(mut guard) => guard.remove(&connection_id).is_some(),
        Err(_) => {
            return abi::error(
                "connector.statePoisoned",
                "Connector connection state is poisoned.",
            )
        }
    };
    abi::ok(Map::from_iter([
        ("connectionId".to_string(), Value::String(connection_id)),
        ("closed".to_string(), Value::Bool(closed)),
    ]))
}

impl OracleConfig {
    fn from_request(request: &Value) -> Result<(Config, Self), String> {
        let mut user = option_string(request, &["user", "username"]).unwrap_or_default();
        let mut password = option_string(request, &["password"]).unwrap_or_default();
        let url = option_string(request, &["url", "connectionString", "dsn"]);
        let wallet_path = option_string(request, &["wallet", "walletPath"]);
        let wallet_password = option_string(request, &["walletPassword", "privateKeyPassphrase"]);
        let tls_requested = option_string(request, &["tls", "ssl", "tlsMode"])
            .map(|value| {
                let value = value.to_ascii_lowercase();
                value == "true"
                    || value == "require"
                    || value == "verifyca"
                    || value == "verifyfull"
            })
            .unwrap_or(false);
        let mut redaction_values = Vec::new();

        let driver_config = if let Some(url) = url.as_ref() {
            let (wallet_from_url, wallet_password_from_url) = parse_wallet_params(url);
            let parsed = parse_connect_string(url);
            push_sensitive(&mut redaction_values, parsed.password.as_deref());
            if user.is_empty() {
                user = parsed.user.unwrap_or_default();
            }
            if password.is_empty() {
                password = parsed.password.unwrap_or_default();
            }
            let mut config = Config::from_str(&parsed.target)
                .map_err(|err| format!("invalid Oracle connection string: {err}"))?;
            config.set_username(user.clone());
            config.set_password(password.clone());
            let wallet = wallet_path.or(wallet_from_url);
            let wallet_password = wallet_password.clone().or(wallet_password_from_url);
            push_sensitive(&mut redaction_values, wallet_password.as_deref());
            if let Some(wallet) = wallet {
                config = config
                    .with_wallet(wallet, wallet_password.as_deref())
                    .map_err(|err| format!("Oracle wallet configuration failed: {err}"))?;
            } else if tls_requested {
                config = config
                    .with_tls()
                    .map_err(|err| format!("Oracle TLS configuration failed: {err}"))?;
            }
            config
        } else {
            let host = option_string(request, &["host"]).unwrap_or_else(|| "localhost".to_string());
            let port = option_u16(request, &["port"]).unwrap_or(1521);
            let service = option_string(request, &["database", "service", "serviceName"]);
            let sid = option_string(request, &["sid"]);
            let mut config = match (service.as_deref(), sid) {
                (Some(service), _) if service.starts_with("sid:") => Config::with_sid(
                    host,
                    port,
                    &service["sid:".len()..],
                    user.clone(),
                    password.clone(),
                ),
                (None, Some(sid)) => {
                    Config::with_sid(host, port, sid, user.clone(), password.clone())
                }
                (service, _) => {
                    let service = service.unwrap_or("FREEPDB1");
                    let service = service.strip_prefix("service:").unwrap_or(service);
                    Config::new(host, port, service, user.clone(), password.clone())
                }
            };
            push_sensitive(&mut redaction_values, wallet_password.as_deref());
            if let Some(wallet) = wallet_path {
                config = config
                    .with_wallet(wallet, wallet_password.as_deref())
                    .map_err(|err| format!("Oracle wallet configuration failed: {err}"))?;
            } else if tls_requested || port == 2484 {
                config = config
                    .with_tls()
                    .map_err(|err| format!("Oracle TLS configuration failed: {err}"))?;
            }
            config
        };
        // Without this the driver sends an empty login and fails on the reply
        // with "buffer underflow", which tells nobody what to fix.
        if user.is_empty() {
            return Err(
                "Oracle needs a user name: enter one in the connection form, or put it in the connection string."
                    .to_string(),
            );
        }
        push_sensitive(&mut redaction_values, Some(&password));
        let target = driver_config.build_connect_string();
        // This connector runs each statement once and closes its cursor, so
        // there is nothing for a statement cache to reuse.
        let driver_config = driver_config.with_statement_cache_size(0);
        Ok((
            driver_config,
            Self {
                target,
                username: user,
                redaction_values,
            },
        ))
    }

    fn redact(&self, message: &str) -> String {
        abi::redact(message, &self.redaction_values)
    }
}

async fn version(connection: &OracleConnection) -> Result<String, String> {
    let guard = connection.conn.lock().await;
    let result = guard
        .query("select banner from v$version where rownum = 1", &[])
        .await
        .map_err(|err| format!("Oracle version query failed: {}", describe_error(&err)))?;
    if result.has_more_rows {
        guard.release_cursor(result.cursor_id);
    }
    Ok(result
        .rows
        .first()
        .and_then(|row| row.get(0))
        .and_then(OraValue::as_str)
        .unwrap_or("Oracle")
        .trim()
        .to_string())
}

async fn run_query(
    connection: &OracleConnection,
    sql: &str,
    cap: usize,
) -> Result<QueryOutput, String> {
    let sql = executable_sql(sql);
    let guard = connection.conn.lock().await;
    if is_explain_plan(sql) {
        guard
            .execute(sql, &[])
            .await
            .map_err(|err| format!("Oracle explain plan failed: {}", describe_error(&err)))?;
        let (columns, rows, truncated) = read_rows(
            &guard,
            "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY)",
            cap,
        )
        .await
        .map_err(|err| format!("Oracle explain plan read failed: {}", describe_error(&err)))?;
        return rows_to_output(&guard, columns, rows, truncated).await;
    }

    let result = guard
        .execute(sql, &[])
        .await
        .map_err(|err| format!("Oracle query failed: {}", describe_error(&err)))?;
    if result.columns.is_empty() {
        settle_transaction(connection, &guard, sql)
            .await
            .map_err(|err| format!("Oracle commit failed: {}", describe_error(&err)))?;
        return Ok(statement_output(sql, result.rows_affected));
    }

    let (columns, rows, truncated) = drain_rows(&guard, result, cap)
        .await
        .map_err(|err| format!("Oracle fetch failed: {}", describe_error(&err)))?;
    rows_to_output(&guard, columns, rows, truncated).await
}

/// Commit what a statement just did, unless the user is managing the transaction.
///
/// Every other engine in the app commits each statement, and the app has no
/// commit button: left open, an `UPDATE` would hold its row locks until the tab
/// closed and then vanish. `SAVEPOINT` and `SET TRANSACTION` are how to opt out,
/// and `COMMIT` or `ROLLBACK` end that.
async fn settle_transaction(
    connection: &OracleConnection,
    conn: &OraConn,
    sql: &str,
) -> oracle_rs::Result<()> {
    match transaction_effect(sql) {
        TransactionEffect::Opens => {
            connection.manual_transaction.store(true, Ordering::Relaxed);
        }
        TransactionEffect::Closes => {
            connection
                .manual_transaction
                .store(false, Ordering::Relaxed);
        }
        TransactionEffect::None => {
            if !connection.manual_transaction.load(Ordering::Relaxed) {
                conn.commit().await?;
            }
        }
    }
    Ok(())
}

/// What a statement with no result set shows: how many rows DML changed.
fn statement_output(sql: &str, rows_affected: u64) -> QueryOutput {
    let first_word = sql
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .find(|word| !word.is_empty())
        .unwrap_or_default();
    let is_dml = ["insert", "update", "delete", "merge"]
        .iter()
        .any(|word| first_word.eq_ignore_ascii_case(word));
    if is_dml {
        (
            vec!["ROWS_AFFECTED".to_string()],
            vec![vec![json!(rows_affected)]],
            false,
        )
    } else {
        (Vec::new(), Vec::new(), false)
    }
}

async fn read_rows(
    conn: &OraConn,
    sql: &str,
    cap: usize,
) -> oracle_rs::Result<(Vec<ColumnInfo>, Vec<Row>, bool)> {
    let result = conn.query(sql, &[]).await?;
    drain_rows(conn, result, cap).await
}

/// Fetch a query's rows up to `cap`, one over so a caller can tell there were more.
async fn drain_rows(
    conn: &OraConn,
    result: oracle_rs::QueryResult,
    cap: usize,
) -> oracle_rs::Result<(Vec<ColumnInfo>, Vec<Row>, bool)> {
    let cursor_id = result.cursor_id;
    let columns = result.columns;
    let mut rows = result.rows;
    let mut more = result.has_more_rows;
    while more && rows.len() <= cap {
        let next = conn
            .fetch_more_after(cursor_id, &columns, FETCH_BATCH, rows.last())
            .await?;
        more = next.has_more_rows;
        rows.extend(next.rows);
    }
    if more {
        conn.release_cursor(cursor_id);
    }
    let truncated = more || rows.len() > cap;
    rows.truncate(cap);
    Ok((columns, rows, truncated))
}

async fn rows_to_output(
    conn: &OraConn,
    columns: Vec<ColumnInfo>,
    rows: Vec<Row>,
    truncated: bool,
) -> Result<QueryOutput, String> {
    let mut output = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut cells = Vec::with_capacity(columns.len());
        for (index, column) in columns.iter().enumerate() {
            cells.push(cell_to_json(conn, column, row.get(index)).await?);
        }
        output.push(cells);
    }
    Ok((
        columns.into_iter().map(|column| column.name).collect(),
        output,
        truncated,
    ))
}

async fn cell_to_json(
    conn: &OraConn,
    column: &ColumnInfo,
    value: Option<&OraValue>,
) -> Result<Value, String> {
    match value {
        Some(OraValue::Lob(LobValue::Locator(locator))) if !locator.is_bfile() => {
            read_lob_cell(conn, locator).await
        }
        Some(OraValue::String(text))
            if matches!(
                column.oracle_type,
                OracleType::Number | OracleType::BinaryInteger
            ) =>
        {
            Ok(number_to_json(text))
        }
        other => Ok(value_to_json(other)),
    }
}

async fn read_lob_cell(conn: &OraConn, locator: &LobLocator) -> Result<Value, String> {
    let total = locator.size();
    if total == 0 {
        return Ok(Value::String(String::new()));
    }
    let data = conn
        .read_lob_range(locator, 1, total.min(MAX_LOB_CELL))
        .await
        .map_err(|err| format!("Oracle LOB read failed: {}", describe_error(&err)))?;
    let mut text = match data {
        LobData::String(text) => text,
        LobData::Bytes(bytes) => format!("\\x{}", abi::hex_encode(&bytes)),
    };
    if total > MAX_LOB_CELL {
        text.push_str(&format!(
            "... [{total} total, showing the first {MAX_LOB_CELL}]"
        ));
    }
    Ok(Value::String(text))
}

/// A NUMBER as a JSON number when that keeps every digit, otherwise its text.
fn number_to_json(text: &str) -> Value {
    if let Ok(integer) = text.parse::<i64>() {
        if integer.abs() <= MAX_SAFE_INTEGER {
            return json!(integer);
        }
        return Value::String(text.to_string());
    }
    match text.parse::<f64>() {
        Ok(float) if float.is_finite() && float.to_string() == text => json!(float),
        _ => Value::String(text.to_string()),
    }
}

/// The driver's error text, with the server's own `ORA-` code stated once.
fn describe_error(err: &OraError) -> String {
    match err {
        OraError::OracleError { code, message } => {
            let message = message.trim();
            if message.starts_with("ORA-") {
                message.to_string()
            } else {
                format!("ORA-{code:05}: {message}")
            }
        }
        OraError::InvalidServiceName { service_name, .. } => format!(
            "ORA-12514: the listener does not know the service name \"{}\". \
             `lsnrctl services` on the server lists the ones it does.",
            service_name.as_deref().unwrap_or_default()
        ),
        OraError::InvalidSid { sid, .. } => format!(
            "ORA-12505: the listener does not know the SID \"{}\". \
             If this database uses a service name, enter that without the `sid:` prefix.",
            sid.as_deref().unwrap_or_default()
        ),
        OraError::ConnectionRefused {
            error_code: Some(code),
            ..
        } => format!("ORA-{code:05}: the listener refused the connection."),
        other => other.to_string(),
    }
}

async fn load_metadata(connection: &OracleConnection) -> Result<Value, String> {
    let guard = connection.conn.lock().await;
    let schema = scalar_string(&guard, "select user from dual")
        .await?
        .unwrap_or_else(|| connection.config.username.clone());
    let mut objects = BTreeMap::<String, ObjectMeta>::new();

    let object_rows = select_all(
        &guard,
        r#"
            select table_name, 'table' as object_type from user_tables
            union all
            select view_name as table_name, 'view' as object_type from user_views
            order by table_name
            "#,
    )
    .await
    .map_err(|err| format!("Oracle metadata objects failed: {err}"))?;
    for row in &object_rows {
        let Some(name) = row_string(row.get(0)) else {
            continue;
        };
        objects.entry(name).or_insert_with(|| ObjectMeta {
            kind: row_string(row.get(1)).unwrap_or_else(|| "table".to_string()),
            ..Default::default()
        });
    }

    let column_rows = select_all(
        &guard,
        r#"
            select table_name, column_name, data_type, nullable, column_id, data_default
            from user_tab_columns
            order by table_name, column_id
            "#,
    )
    .await
    .map_err(|err| format!("Oracle metadata columns failed: {err}"))?;
    for row in &column_rows {
        let table = row_string(row.get(0)).unwrap_or_default();
        let Some(object) = objects.get_mut(&table) else {
            continue;
        };
        object.columns.push(json!({
            "name": row_string(row.get(1)).unwrap_or_default(),
            "dataType": row_string(row.get(2)).unwrap_or_default(),
            "nullable": row_string(row.get(3)).as_deref() == Some("Y"),
            "ordinal": row_i64(row.get(4)).unwrap_or((object.columns.len() + 1) as i64),
            "default": row_string(row.get(5))
        }));
    }

    let index_rows = select_all(
        &guard,
        r#"
            select i.table_name,
                   i.index_name,
                   i.uniqueness,
                   listagg(c.column_name, ',') within group (order by c.column_position) as columns
            from user_indexes i
            join user_ind_columns c on c.index_name = i.index_name
            group by i.table_name, i.index_name, i.uniqueness
            order by i.table_name, i.index_name
            "#,
    )
    .await
    .map_err(|err| format!("Oracle metadata indexes failed: {err}"))?;
    for row in &index_rows {
        let table = row_string(row.get(0)).unwrap_or_default();
        let Some(object) = objects.get_mut(&table) else {
            continue;
        };
        let columns = row_string(row.get(3)).unwrap_or_default();
        object.indexes.push(json!({
            "name": row_string(row.get(1)).unwrap_or_default(),
            "columns": columns
                .split(',')
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>(),
            "unique": row_string(row.get(2)).as_deref() == Some("UNIQUE")
        }));
    }

    let pk_rows = select_all(
        &guard,
        r#"
            select a.table_name, a.column_name
            from user_cons_columns a
            join user_constraints c on a.constraint_name = c.constraint_name
            where c.constraint_type = 'P'
            order by a.table_name, a.position
            "#,
    )
    .await
    .map_err(|err| format!("Oracle metadata primary keys failed: {err}"))?;
    for row in &pk_rows {
        let table = row_string(row.get(0)).unwrap_or_default();
        let column = row_string(row.get(1)).unwrap_or_default();
        if let Some(object) = objects.get_mut(&table) {
            object.primary_key.push(column);
        }
    }

    let fk_rows = select_all(&guard, r#"
            select a.table_name, a.constraint_name, a.column_name,
                   c_pk.table_name as ref_table, b.column_name as ref_column,
                   c_pk.owner as ref_owner
            from user_cons_columns a
            join user_constraints c on a.constraint_name = c.constraint_name
            join user_constraints c_pk on c.r_constraint_name = c_pk.constraint_name
            join user_cons_columns b on c_pk.constraint_name = b.constraint_name and a.position = b.position
            where c.constraint_type = 'R'
            order by a.table_name, a.constraint_name, a.position
            "#)
        .await
        .map_err(|err| format!("Oracle metadata foreign keys failed: {err}"))?;
    let mut fk_by_key = BTreeMap::<(String, String), usize>::new();
    for row in &fk_rows {
        let table = row_string(row.get(0)).unwrap_or_default();
        let constraint = row_string(row.get(1)).unwrap_or_default();
        let Some(object) = objects.get_mut(&table) else {
            continue;
        };
        let index = *fk_by_key
            .entry((table.clone(), constraint.clone()))
            .or_insert_with(|| {
                object.foreign_keys.push(json!({
                    "name": constraint,
                    "columns": [],
                    "referencesSchema": row_string(row.get(5)),
                    "referencesTable": row_string(row.get(3)).unwrap_or_default(),
                    "referencesColumns": []
                }));
                object.foreign_keys.len() - 1
            });
        if let Some(foreign_key) = object
            .foreign_keys
            .get_mut(index)
            .and_then(Value::as_object_mut)
        {
            push_json_string(
                foreign_key,
                "columns",
                row_string(row.get(2)).unwrap_or_default(),
            );
            push_json_string(
                foreign_key,
                "referencesColumns",
                row_string(row.get(4)).unwrap_or_default(),
            );
        }
    }

    let routine_rows = select_all(
        &guard,
        r#"
            select object_name, procedure_name, object_type
            from user_procedures
            where object_type in ('PROCEDURE', 'FUNCTION', 'PACKAGE')
            order by object_name, procedure_name
            "#,
    )
    .await
    .map_err(|err| format!("Oracle metadata routines failed: {err}"))?;
    for row in &routine_rows {
        let object_name = row_string(row.get(0)).unwrap_or_default();
        let procedure_name = row_string(row.get(1));
        let object_type = row_string(row.get(2)).unwrap_or_default();
        if procedure_name.is_none() && object_type == "PACKAGE" {
            continue;
        }
        let name = procedure_name
            .map(|procedure| format!("{object_name}.{procedure}"))
            .unwrap_or(object_name);
        objects.entry(name).or_insert_with(|| ObjectMeta {
            kind: if object_type == "FUNCTION" {
                "function".to_string()
            } else {
                "procedure".to_string()
            },
            ..Default::default()
        });
    }

    Ok(json!({
        "schemas": [{
            "name": schema,
            "objects": objects
                .into_iter()
                .map(|(name, object)| json!({
                    "schema": schema,
                    "name": name,
                    "kind": object.kind,
                    "columns": object.columns,
                    "indexes": object.indexes,
                    "primaryKey": object.primary_key,
                    "foreignKeys": object.foreign_keys
                }))
                .collect::<Vec<_>>()
        }]
    }))
}

async fn scalar_string(conn: &OraConn, sql: &str) -> Result<Option<String>, String> {
    let result = conn
        .query(sql, &[])
        .await
        .map_err(|err| format!("Oracle scalar query failed: {}", describe_error(&err)))?;
    if result.has_more_rows {
        conn.release_cursor(result.cursor_id);
    }
    Ok(result.rows.first().and_then(|row| row_string(row.get(0))))
}

/// Every row of a dictionary query: a schema can have far more than one batch.
async fn select_all(conn: &OraConn, sql: &str) -> Result<Vec<Row>, String> {
    let (_, rows, _) = read_rows(conn, sql, usize::MAX - 1)
        .await
        .map_err(|err| describe_error(&err))?;
    Ok(rows)
}

fn parse_wallet_params(url: &str) -> (Option<String>, Option<String>) {
    let mut wallet_path = None;
    let mut wallet_password = None;
    if let Some(pos) = url.find('?') {
        for pair in url[pos + 1..].split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                if key == "wallet" {
                    wallet_path = Some(percent_decode(value, true));
                } else if key == "wallet_password" {
                    wallet_password = Some(percent_decode(value, true));
                }
            }
        }
    }
    (wallet_path, wallet_password)
}

fn value_to_json(value: Option<&OraValue>) -> Value {
    match value {
        None | Some(OraValue::Null) => Value::Null,
        Some(OraValue::Boolean(value)) => Value::Bool(*value),
        Some(OraValue::Integer(value)) => json!(value),
        Some(OraValue::Float(value)) => json!(value),
        Some(OraValue::String(value)) => Value::String(value.clone()),
        Some(OraValue::Bytes(value)) => Value::String(format!("\\x{}", abi::hex_encode(value))),
        Some(OraValue::Json(value)) => value.clone(),
        Some(OraValue::Number(value)) => number_to_json(value.as_str()),
        Some(OraValue::Lob(LobValue::Null)) => Value::Null,
        Some(OraValue::Lob(LobValue::Empty)) => Value::String(String::new()),
        Some(OraValue::Lob(LobValue::Inline(data))) => match std::str::from_utf8(data) {
            Ok(text) => Value::String(text.to_string()),
            Err(_) => Value::String(format!("\\x{}", abi::hex_encode(data))),
        },
        Some(other) => Value::String(other.to_string()),
    }
}

fn row_string(value: Option<&OraValue>) -> Option<String> {
    match value? {
        OraValue::Null => None,
        other => Some(other.to_string()),
    }
}

fn row_i64(value: Option<&OraValue>) -> Option<i64> {
    value.and_then(OraValue::as_i64)
}

fn push_json_string(object: &mut Map<String, Value>, key: &str, value: String) {
    if let Some(values) = object.get_mut(key).and_then(Value::as_array_mut) {
        values.push(Value::String(value));
    }
}

fn connection(connection_id: &str) -> Result<OracleConnection, IrodoriConnectorBuffer> {
    let guard = connections().lock().map_err(|_| {
        abi::error(
            "connector.statePoisoned",
            "Connector connection state is poisoned.",
        )
    })?;
    guard.get(connection_id).cloned().ok_or_else(|| {
        abi::error(
            "connector.connectionNotFound",
            format!("no open connection: {connection_id}"),
        )
    })
}

fn option_u16(request: &Value, fields: &[&str]) -> Option<u16> {
    request_containers(request)
        .into_iter()
        .find_map(|container| {
            fields.iter().find_map(|field| {
                container
                    .get(*field)
                    .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
                    .and_then(|value| u16::try_from(value).ok())
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_wallet_params() {
        let (wallet, password) =
            parse_wallet_params("//db:2484/service?wallet=%2Ftmp%2Fwallet&wallet_password=a+b");
        assert_eq!(wallet.as_deref(), Some("/tmp/wallet"));
        assert_eq!(password.as_deref(), Some("a b"));
    }

    #[test]
    fn maps_oracle_values_to_json() {
        assert_eq!(value_to_json(Some(&OraValue::Integer(7))), json!(7));
        assert_eq!(
            value_to_json(Some(&OraValue::Bytes(vec![0xde, 0xad]))),
            json!("\\xdead")
        );
        assert_eq!(value_to_json(Some(&OraValue::Null)), Value::Null);
    }

    #[test]
    fn a_number_stays_a_number_only_while_it_keeps_every_digit() {
        assert_eq!(number_to_json("42"), json!(42));
        assert_eq!(number_to_json("-1.5"), json!(-1.5));
        assert_eq!(
            number_to_json("9007199254740991"),
            json!(9007199254740991_i64)
        );
        assert_eq!(
            number_to_json("9007199254740992"),
            json!("9007199254740992")
        );
        assert_eq!(
            number_to_json("12345678901234567890"),
            json!("12345678901234567890")
        );
        assert_eq!(
            number_to_json("0.1234567890123456789"),
            json!("0.1234567890123456789")
        );
    }

    #[test]
    fn states_the_server_error_code_once() {
        let error = |message: &str| OraError::OracleError {
            code: 942,
            message: message.to_string(),
        };
        assert_eq!(
            describe_error(&error("ORA-00942: table or view does not exist\n")),
            "ORA-00942: table or view does not exist"
        );
        assert_eq!(describe_error(&error("no text")), "ORA-00942: no text");
        let unknown_service = describe_error(&OraError::InvalidServiceName {
            service_name: Some("NOPE".to_string()),
            message: Some("(DESCRIPTION=(ERR=12514))".to_string()),
        });
        assert!(
            unknown_service.starts_with("ORA-12514"),
            "{unknown_service}"
        );
        assert!(unknown_service.contains("\"NOPE\""), "{unknown_service}");
    }

    fn connect_request(fields: Value) -> Value {
        json!({ "method": "connect", "connectionId": "c", "profile": fields })
    }

    #[test]
    fn reads_the_service_name_or_sid_from_the_database_field() {
        let target = |database: &str| {
            let request =
                connect_request(json!({ "host": "db", "user": "u", "database": database }));
            OracleConfig::from_request(&request).unwrap().1.target
        };
        assert!(target("FREEPDB1").contains("(SERVICE_NAME=FREEPDB1)"));
        assert!(target("service:SVC").contains("(SERVICE_NAME=SVC)"));
        assert!(target("sid:ORCL").contains("(SID=ORCL)"));

        let request = connect_request(json!({ "host": "db", "user": "u", "sid": "ORCL" }));
        let config = OracleConfig::from_request(&request).unwrap().1;
        assert!(config.target.contains("(SID=ORCL)"));
    }

    #[test]
    fn asks_for_a_user_instead_of_failing_on_the_reply() {
        let request = connect_request(json!({ "host": "db", "database": "SVC" }));
        let error = OracleConfig::from_request(&request).err().unwrap();
        assert!(error.contains("needs a user name"), "{error}");
    }

    #[test]
    fn takes_credentials_from_the_connection_string_and_redacts_them() {
        let request = connect_request(json!({ "url": "oracle://scott:s3cret@db:1521/SVC" }));
        let config = OracleConfig::from_request(&request).unwrap().1;
        assert_eq!(config.username, "scott");
        assert!(config.target.contains("(SERVICE_NAME=SVC)"));
        assert_eq!(config.redact("bad password s3cret"), "bad password ****");

        // The profile's fields win over the string's.
        let request = connect_request(
            json!({ "url": "oracle://scott:tiger@db/SVC", "user": "hr", "password": "pw" }),
        );
        let config = OracleConfig::from_request(&request).unwrap().1;
        assert_eq!(config.username, "hr");
    }
}

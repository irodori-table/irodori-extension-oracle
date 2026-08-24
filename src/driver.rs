use irodori_connector_abi::{option_string, push_sensitive, request_containers};
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use oracle_rs::{Config, Connection as OraConn, Value as OraValue};
use serde_json::{json, Map, Value};
use tokio::runtime::Runtime;
use tokio::sync::Mutex as AsyncMutex;

use crate::abi::{self, IrodoriConnectorBuffer};
use crate::{ABI_VERSION, CONFIG_JSON, DRIVER_LINKED, ENGINE, MANIFEST_JSON};

static CONNECTIONS: OnceLock<Mutex<HashMap<String, OracleConnection>>> = OnceLock::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[derive(Clone)]
struct OracleConnection {
    conn: Arc<AsyncMutex<OraConn>>,
    config: OracleConfig,
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
            .map_err(|err| err.to_string())
    }) {
        Ok(conn) => conn,
        Err(err) => return abi::error("connector.connectFailed", connector_config.redact(&err)),
    };
    let connection = OracleConnection {
        conn: Arc::new(AsyncMutex::new(conn)),
        config: connector_config,
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
        let user = option_string(request, &["user", "username"]).unwrap_or_default();
        let password = option_string(request, &["password"]).unwrap_or_default();
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

        let driver_config = if let Some(url) = url.as_ref() {
            let (wallet_from_url, wallet_password_from_url) = parse_wallet_params(url);
            let clean_url = url.split('?').next().unwrap_or(url);
            let mut config =
                Config::from_str(clean_url).map_err(|err| format!("invalid Oracle URL: {err}"))?;
            if !user.is_empty() {
                config.set_username(user.clone());
            }
            if !password.is_empty() {
                config.set_password(password.clone());
            }
            let wallet = wallet_path.or(wallet_from_url);
            let wallet_password = wallet_password.clone().or(wallet_password_from_url);
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
            let database = option_string(request, &["database", "service", "serviceName", "sid"])
                .unwrap_or_else(|| "FREEPDB1".to_string());
            let mut config = if let Some(sid) = database.strip_prefix("sid:") {
                Config::with_sid(host, port, sid, user.clone(), password.clone())
            } else {
                let service = database.strip_prefix("service:").unwrap_or(&database);
                Config::new(host, port, service, user.clone(), password.clone())
            };
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
        let mut redaction_values = Vec::new();
        push_sensitive(&mut redaction_values, Some(&password));
        push_sensitive(&mut redaction_values, wallet_password.as_deref());
        let target = driver_config.build_connect_string();
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
        .map_err(|err| format!("Oracle version query failed: {err}"))?;
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
    let guard = connection.conn.lock().await;
    let result = if sql
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("explain plan for")
    {
        guard
            .query(sql, &[])
            .await
            .map_err(|err| format!("Oracle explain plan failed: {err}"))?;
        guard
            .query(
                "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY)",
                &[],
            )
            .await
            .map_err(|err| format!("Oracle explain plan read failed: {err}"))?
    } else {
        guard
            .query(sql, &[])
            .await
            .map_err(|err| format!("Oracle query failed: {err}"))?
    };
    let columns = result
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let truncated = result.has_more_rows || result.rows.len() > cap;
    let rows = result
        .rows
        .iter()
        .take(cap)
        .map(|row| {
            (0..columns.len())
                .map(|index| value_to_json(row.get(index)))
                .collect()
        })
        .collect();
    Ok((columns, rows, truncated))
}

async fn load_metadata(connection: &OracleConnection) -> Result<Value, String> {
    let guard = connection.conn.lock().await;
    let schema = scalar_string(&guard, "select user from dual")
        .await?
        .unwrap_or_else(|| connection.config.username.clone());
    let mut objects = BTreeMap::<String, ObjectMeta>::new();

    let object_rows = guard
        .query(
            r#"
            select table_name, 'table' as object_type from user_tables
            union all
            select view_name as table_name, 'view' as object_type from user_views
            order by table_name
            "#,
            &[],
        )
        .await
        .map_err(|err| format!("Oracle metadata objects failed: {err}"))?;
    for row in &object_rows.rows {
        let Some(name) = row_string(row.get(0)) else {
            continue;
        };
        objects.entry(name).or_insert_with(|| ObjectMeta {
            kind: row_string(row.get(1)).unwrap_or_else(|| "table".to_string()),
            ..Default::default()
        });
    }

    let column_rows = guard
        .query(
            r#"
            select table_name, column_name, data_type, nullable, column_id, data_default
            from user_tab_columns
            order by table_name, column_id
            "#,
            &[],
        )
        .await
        .map_err(|err| format!("Oracle metadata columns failed: {err}"))?;
    for row in &column_rows.rows {
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

    let index_rows = guard
        .query(
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
            &[],
        )
        .await
        .map_err(|err| format!("Oracle metadata indexes failed: {err}"))?;
    for row in &index_rows.rows {
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

    let pk_rows = guard
        .query(
            r#"
            select a.table_name, a.column_name
            from user_cons_columns a
            join user_constraints c on a.constraint_name = c.constraint_name
            where c.constraint_type = 'P'
            order by a.table_name, a.position
            "#,
            &[],
        )
        .await
        .map_err(|err| format!("Oracle metadata primary keys failed: {err}"))?;
    for row in &pk_rows.rows {
        let table = row_string(row.get(0)).unwrap_or_default();
        let column = row_string(row.get(1)).unwrap_or_default();
        if let Some(object) = objects.get_mut(&table) {
            object.primary_key.push(column);
        }
    }

    let fk_rows = guard
        .query(
            r#"
            select a.table_name, a.constraint_name, a.column_name,
                   c_pk.table_name as ref_table, b.column_name as ref_column,
                   c_pk.owner as ref_owner
            from user_cons_columns a
            join user_constraints c on a.constraint_name = c.constraint_name
            join user_constraints c_pk on c.r_constraint_name = c_pk.constraint_name
            join user_cons_columns b on c_pk.constraint_name = b.constraint_name and a.position = b.position
            where c.constraint_type = 'R'
            order by a.table_name, a.constraint_name, a.position
            "#,
            &[],
        )
        .await
        .map_err(|err| format!("Oracle metadata foreign keys failed: {err}"))?;
    let mut fk_by_key = BTreeMap::<(String, String), usize>::new();
    for row in &fk_rows.rows {
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

    let routine_rows = guard
        .query(
            r#"
            select object_name, procedure_name, object_type
            from user_procedures
            where object_type in ('PROCEDURE', 'FUNCTION', 'PACKAGE')
            order by object_name, procedure_name
            "#,
            &[],
        )
        .await
        .map_err(|err| format!("Oracle metadata routines failed: {err}"))?;
    for row in &routine_rows.rows {
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
        .map_err(|err| format!("Oracle scalar query failed: {err}"))?;
    Ok(result.rows.first().and_then(|row| row_string(row.get(0))))
}

fn parse_wallet_params(url: &str) -> (Option<String>, Option<String>) {
    let mut wallet_path = None;
    let mut wallet_password = None;
    if let Some(pos) = url.find('?') {
        for pair in url[pos + 1..].split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                if key == "wallet" {
                    wallet_path = Some(percent_decode(value));
                } else if key == "wallet_password" {
                    wallet_password = Some(percent_decode(value));
                }
            }
        }
    }
    (wallet_path, wallet_password)
}

fn percent_decode(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let h1 = chars.next().unwrap_or('0');
            let h2 = chars.next().unwrap_or('0');
            if let Ok(byte) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                out.push(byte as char);
            }
        } else if ch == '+' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
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
        Some(OraValue::Number(_)) => value
            .and_then(OraValue::as_i64)
            .map(|value| json!(value))
            .or_else(|| value.and_then(OraValue::as_f64).map(|value| json!(value)))
            .unwrap_or_else(|| Value::String(format!("{:?}", value.unwrap()))),
        Some(other) => Value::String(format!("{other:?}")),
    }
}

fn row_string(value: Option<&OraValue>) -> Option<String> {
    match value? {
        OraValue::Null => None,
        OraValue::String(value) => Some(value.clone()),
        OraValue::Integer(value) => Some(value.to_string()),
        OraValue::Float(value) => Some(value.to_string()),
        OraValue::Number(value) => Some(format!("{value:?}")),
        other => Some(format!("{other:?}")),
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
    }
}

<!-- i18n: language-switcher -->
[English](README.md) | [日本語](README.ja.md)

# Oracle Connector

Native Irodori Table connector extension for Oracle.

This crate packages the connector metadata, native ABI exports, and driver implementation used by the Irodori extension marketplace.

## Connector

- Extension ID: `irodori.oracle`
- Engine ID: `oracle`
- Wire protocol: `oracle`
- Default port: `1521`
- Native ABI: `irodori.connector.native.v1`
- Driver linked: `yes`
- Marketplace visibility: `public`
- Package version: `0.1.8`

The package includes a desktop adapter source snapshot from `db/oracle.rs`.

Connector metadata lives in `connector.config.json` and `irodori.extension.json`.
The Rust crate exports the native ABI from `src/lib.rs`, uses `irodori-connector-abi` for shared JSON/buffer helpers, and keeps connector behavior in `src/driver.rs`.

## Connection Metadata

- Endpoint modes: `hostPort`, `connectionString`
- Transport modes: `direct`, `sshTunnel`, `socks5Proxy`, `httpConnectProxy`, `proxyChain`
- TLS supported: `yes`
- TLS required by default: `no`
- Custom driver options: `yes`

### Endpoint Fields

| Field | Label | Type | Required |
| --- | --- | --- | --- |
| `host` | Host | `string` | yes |
| `port` | Port | `number` | no |
| `database` | Service name / SID | `string` | no |

## Authentication

The connector advertises these authentication modes so clients can render the right credential fields. Driver-specific or provider-specific values can still be passed through `options` when needed.

| Auth method | Label | Kind | Secret purposes |
| --- | --- | --- | --- |
| `userPassword` | User/password | `userPassword` | `password` |
| `connectionString` | Connection string / DSN | `connectionString` | none |
| `oracleWallet` | Oracle wallet / mTLS | `certificate` | `password`, `privateKeyPassphrase` |
| `customDriverOptions` | Custom driver options | `custom` | `password`, `token`, `privateKey`, `privateKeyPassphrase` |
| `kerberos` | Kerberos / GSSAPI | `kerberos` | `token` |
| `cloudIamToken` | Cloud IAM token | `token` | `token` |
| `accessToken` | Access token | `token` | `token` |
| `oauth2` | OAuth 2.0 | `oauth2` | `token` |

`kerberos`, `cloudIamToken`, `accessToken`, and `oauth2` are declared but not
implemented yet ([#6](https://github.com/irodori-table/irodori-extension-oracle/issues/6)).

## Connecting

Enter the service name in **Service name / SID**, or `sid:ORCL` for a database
known by its SID. Left empty, it is `FREEPDB1`.

A connection string can be any of these. Credentials in it apply unless the
profile sets its own.

```text
oracle://user:password@host:1521/service   (percent-encode special characters)
oracle://user:password@host:1521/sid:ORCL
jdbc:oracle:thin:user/password@//host:1521/service
jdbc:oracle:thin:@host:1521:ORCL
user/password@host:1521/service
//host:1521/service
```

Add `?wallet=/path/to/wallet&wallet_password=...` to any of them for a wallet.

## Transactions

Each statement commits as it succeeds, as on the app's other engines. To manage a
transaction by hand, start with `SET TRANSACTION` or `SAVEPOINT`: statements then
wait for your `COMMIT` or `ROLLBACK`. Closing the connection before that rolls back.

## Testing against a server

`tests/live_oracle.rs` drives the connector through its ABI and is skipped unless
`IRODORI_ORACLE` is set:

```sh
docker run -d -p 55521:1521 -e ORACLE_PASSWORD=irodori \
  -e APP_USER=irodori -e APP_USER_PASSWORD=irodori gvenzl/oracle-free:slim-faststart
IRODORI_ORACLE=1 cargo test --test live_oracle -- --test-threads=1
```

`IRODORI_ORACLE_HOST`, `_PORT`, `_USER`, `_PASSWORD`, and `_SERVICE` point it
elsewhere.

## Patched driver

`vendor/oracle-rs-0.1.7-patched` is oracle-rs 0.1.7 with the fixes listed in its
[PATCHES.md](vendor/oracle-rs-0.1.7-patched/PATCHES.md).

## Native ABI Calls

| Method | Response |
| --- | --- |
| `health` | Returns connector health, engine id, ABI version, and driver status. |
| `describe` | Returns the embedded manifest and connector config. |
| `manifest` | Returns raw `irodori.extension.json`. |
| `config` | Returns raw `connector.config.json`. |
| `connect` | Opens and validates a native connector connection. |
| `query` | Runs a connector query and returns structured rows or JSON results. |
| `metadata` | Reads schemas, tables, columns, indexes, collections, or equivalent metadata. |
| `close` | Closes and removes a cached native connection. |

## Development

All extension crates in this checkout share `../target` so dependencies compile once across sibling repositories.

```sh
make check
make build
```

Release packages place platform-specific native artifacts under `dist/native`.

## License

0BSD. You can use, copy, modify, and distribute this project for almost any purpose.

<!-- i18n: language-switcher -->
[English](README.md) | [日本語](README.ja.md)

# Oracle コネクタ

Oracle 用のネイティブ Irodori テーブルコネクタ拡張です。

このクレートは、Irodori 拡張マーケットプレイスで使用されるコネクタのメタデータ、ネイティブ ABI エクスポート、およびドライバー実装をパッケージ化しています。

## コネクタ

- 拡張 ID: `irodori.oracle`
- エンジン ID: `oracle`
- ワイヤープロトコル: `oracle`
- デフォルトポート: `1521`
- ネイティブ ABI: `irodori.connector.native.v1`
- ドライバー連携: `yes`
- マーケットプレイス公開範囲: `public`
- パッケージバージョン: `0.1.8`

パッケージには `db/oracle.rs` からのデスクトップアダプターのソーススナップショットが含まれています。

コネクタのメタデータは `connector.config.json` と `irodori.extension.json` にあります。
Rust クレートは `src/lib.rs` からネイティブ ABI をエクスポートし、共有の JSON/バッファヘルパーに `irodori-connector-abi` を使用し、コネクタの動作は `src/driver.rs` に保持しています。

## 接続メタデータ

- エンドポイントモード: `hostPort`, `connectionString`
- トランスポートモード: `direct`, `sshTunnel`, `socks5Proxy`, `httpConnectProxy`, `proxyChain`
- TLS 対応: `yes`
- デフォルトで TLS 必須: `no`
- カスタムドライバーオプション: `yes`

### エンドポイントフィールド

| フィールド | ラベル | 型 | 必須 |
| --- | --- | --- | --- |
| `host` | ホスト | `string` | yes |
| `port` | ポート | `number` | no |
| `database` | サービス名 / SID | `string` | no |

## 認証

コネクタはこれらの認証モードを公開しており、クライアントは適切な認証情報フィールドを表示できます。
ドライバー固有またはプロバイダー固有の値は必要に応じて `options` 経由で渡すことも可能です。

| 認証方式 | ラベル | 種類 | 秘密情報の用途 |
| --- | --- | --- | --- |
| `userPassword` | ユーザー/パスワード | `userPassword` | `password` |
| `connectionString` | 接続文字列 / DSN | `connectionString` | なし |
| `oracleWallet` | Oracle ウォレット / mTLS | `certificate` | `password`, `privateKeyPassphrase` |
| `customDriverOptions` | カスタムドライバーオプション | `custom` | `password`, `token`, `privateKey`, `privateKeyPassphrase` |
| `kerberos` | Kerberos / GSSAPI | `kerberos` | `token` |
| `cloudIamToken` | クラウド IAM トークン | `token` | `token` |
| `accessToken` | アクセストークン | `token` | `token` |
| `oauth2` | OAuth 2.0 | `oauth2` | `token` |

`kerberos`、`cloudIamToken`、`accessToken`、`oauth2` は宣言のみで未実装です
([#6](https://github.com/irodori-table/irodori-extension-oracle/issues/6))。

## 接続

**サービス名 / SID** にサービス名を入力します。SID で接続するデータベースは
`sid:ORCL` と入力します。空欄の場合は `FREEPDB1` です。

接続文字列は次のいずれの形式でも指定できます。文字列内の資格情報は、プロファイルに
別の値がなければ使われます。

```text
oracle://user:password@host:1521/service   (特殊文字はパーセントエンコード)
oracle://user:password@host:1521/sid:ORCL
jdbc:oracle:thin:user/password@//host:1521/service
jdbc:oracle:thin:@host:1521:ORCL
user/password@host:1521/service
//host:1521/service
```

ウォレットを使うときは `?wallet=/path/to/wallet&wallet_password=...` を付けます。

## トランザクション

アプリの他のエンジンと同じく、各文は成功した時点でコミットされます。手動で管理する
ときは `SET TRANSACTION` または `SAVEPOINT` から始めてください。以降の文は `COMMIT`
または `ROLLBACK` まで確定しません。その前に接続を閉じるとロールバックされます。

## サーバーを使ったテスト

`tests/live_oracle.rs` は ABI 経由でコネクタを動かします。`IRODORI_ORACLE` が
未設定のときはスキップされます。

```sh
docker run -d -p 55521:1521 -e ORACLE_PASSWORD=irodori \
  -e APP_USER=irodori -e APP_USER_PASSWORD=irodori gvenzl/oracle-free:slim-faststart
IRODORI_ORACLE=1 cargo test --test live_oracle -- --test-threads=1
```

接続先は `IRODORI_ORACLE_HOST`、`_PORT`、`_USER`、`_PASSWORD`、`_SERVICE` で変更できます。

## パッチ済みドライバ

`vendor/oracle-rs-0.1.7-patched` は oracle-rs 0.1.7 に
[PATCHES.md](vendor/oracle-rs-0.1.7-patched/PATCHES.md) の修正を加えたものです。

## ネイティブ ABI 呼び出し

| メソッド | レスポンス |
| --- | --- |
| `health` | コネクタのヘルス、エンジン ID、ABI バージョン、ドライバー状態を返します。 |
| `describe` | 埋め込みマニフェストとコネクタ設定を返します。 |
| `manifest` | 生の `irodori.extension.json` を返します。 |
| `config` | 生の `connector.config.json` を返します。 |
| `connect` | ネイティブコネクタ接続を開き、検証します。 |
| `query` | コネクタクエリを実行し、構造化された行または JSON 結果を返します。 |
| `metadata` | スキーマ、テーブル、カラム、インデックス、コレクション、または同等のメタデータを読み取ります。 |
| `close` | キャッシュされたネイティブ接続を閉じて削除します。 |

## 開発

このチェックアウト内のすべての拡張クレートは `../target` を共有しているため、依存関係は兄弟リポジトリ間で一度だけコンパイルされます。

```sh
make check
make build
```

リリースパッケージはプラットフォーム固有のネイティブアーティファクトを `dist/native` に配置します。

## ライセンス

0BSD。ほぼあらゆる目的でこのプロジェクトを使用、コピー、修正、配布できます。
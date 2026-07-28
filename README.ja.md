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
- パッケージバージョン: `0.1.3`

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
| `database` | データベース | `string` | no |

## 認証

コネクタはこれらの認証モードを公開しており、クライアントは適切な認証情報フィールドを表示できます。
ドライバー固有またはプロバイダー固有の値は必要に応じて `options` 経由で渡すことも可能です。

| 認証方式 | ラベル | 種類 | 秘密情報の用途 |
| --- | --- | --- | --- |
| `none` | 認証なし | `none` | なし |
| `connectionString` | 接続文字列 / DSN | `connectionString` | なし |
| `userPassword` | ユーザー/パスワード | `userPassword` | `password` |
| `oracleWallet` | Oracle ウォレット / mTLS | `certificate` | `privateKey`, `privateKeyPassphrase` |
| `kerberos` | Kerberos / GSSAPI | `kerberos` | `token` |
| `cloudIamToken` | クラウド IAM トークン | `token` | `token` |
| `accessToken` | アクセストークン | `token` | `token` |
| `oauth2` | OAuth 2.0 | `oauth2` | `token` |
| `customDriverOptions` | カスタムドライバーオプション | `custom` | `password`, `token`, `privateKey`, `privateKeyPassphrase` |

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
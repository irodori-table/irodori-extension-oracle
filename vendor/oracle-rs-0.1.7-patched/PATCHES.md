# oracle-rs 0.1.7, patched

The published [oracle-rs](https://github.com/stiang/oracle-rs) 0.1.7 (MIT OR
Apache-2.0, see `LICENSE-*`), unchanged except for the fixes below. They are made
against a real server: `tests/live_oracle.rs` in this repository passes on Oracle
26ai Free, 21c XE and 18c XE. Upstream has been quiet since March 2026 and these
problems are all reported there, so this copy exists until a release carries them.
Each one was written against this source, using the linked reports for reference.

| Fix | What it does | Without it | Upstream |
| --- | --- | --- | --- |
| Marker framing | Marker packets use the 4-byte length of a large-SDU connection | Any SQL error closes the connection and the `ORA-` text is lost | [#16](https://github.com/stiang/oracle-rs/pull/16) |
| Error parsing by TTC version | All four error parsers read the 20c fields only on servers that send them | Every query on 19c and older fails with "buffer underflow", and errors on 21c and newer lose their text | [#13](https://github.com/stiang/oracle-rs/issues/13), [#17](https://github.com/stiang/oracle-rs/pull/17), [#24](https://github.com/stiang/oracle-rs/issues/24) |
| Closing cursors | Finished cursors are closed by a piggyback ahead of the next statement | ORA-01000 and an unusable connection after about 300 statements | [#23](https://github.com/stiang/oracle-rs/issues/23) |
| `has_more_rows` | Reports more rows until the server sends ORA-01403 | Results stop at 100 rows with nothing saying so | [#8](https://github.com/stiang/oracle-rs/pull/8) |
| `fetch_more` | Fetch requests use the connection's framing, sequence and token, and rows repeat across a batch boundary through `fetch_more_after` | Fetching a second batch breaks the connection | [#8](https://github.com/stiang/oracle-rs/pull/8), [#9](https://github.com/stiang/oracle-rs/issues/9) |
| Row header bit vector | The duplicate-column map in a row header is kept | Columns the server left out are read from whatever follows | |
| Multi-packet query responses | A query response is read until its terminal message | Rows past one packet are lost and the next call reads their bytes | [#14](https://github.com/stiang/oracle-rs/pull/14) |
| Requests over the SDU | `send` splits a request larger than the SDU | Long statements fail with ORA-12592 or a closed connection | [#21](https://github.com/stiang/oracle-rs/issues/21) |
| LONG columns | The null indicator and return code after a LONG value are read | The connection goes out of step, so `user_tab_columns` and `user_views` break it | |
| NCHAR | NVARCHAR2 and NCHAR decode as UTF-16 | Non-ASCII text is garbled | |
| ROWID | ROWID is read from its parts | `select rowid` returns garbage | |
| BINARY_FLOAT, BINARY_DOUBLE, BOOLEAN, INTERVAL | Each decodes to its value | Garbage strings | |
| TIMESTAMP WITH TIME ZONE | The value shows wall-clock time at its offset, and a named region shows its UTC instant | Times are off by the offset, and a named region fails the statement | |
| Login and listener errors | A rejected login or refused connection reports the server's reason | "authentication rejected" hides ORA-01017 and ORA-28000, and "Connection refused" hides ORA-12514 | |
| Statement classification | `select*from t` and `(select ...)` count as queries | They run as DML and return no rows | |

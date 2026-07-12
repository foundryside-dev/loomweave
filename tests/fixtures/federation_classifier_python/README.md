# Federation classifier golden input

This fixture intentionally contains five independently runnable Python modules.
Each module contributes one `cli-command` and one `entry-point`; none declares
`__all__` or an HTTP route. The normative producer run must therefore report
exact counts `5/5/0/0` for `cli-command`, `entry-point`, `http-route`, and
`exported-api`.

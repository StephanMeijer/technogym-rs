# technogym-mcp

An [MCP](https://modelcontextprotocol.io) server that gives Claude (and other MCP clients) read
access to your Technogym **mywellness** training history. Built on the
[`technogym`](https://crates.io/crates/technogym) crate. Unofficial; not affiliated with Technogym.

## Tools

| Tool | Returns |
|---|---|
| `get_account_info` | User id, name, culture, facilities and Movergy score. Use it to check the login. |
| `get_workout_sessions(from_date?, to_date?, only_workouts=true)` | Sessions in a range (default the last 30 days), newest first. |
| `get_workout_session(session_id, date?, include_raw=false)` | Every exercise with sets, the machine's summary, workload ranges; session efficacy and totals. |
| `get_recent_workouts(days=14)` | `{ days, sessions, omitted, note? }`: the sessions of the last N days, newest first and fully loaded, as many as fit one result. |
| `get_exercise_analytics(analytics_id, sample_every_seconds=10, segment_by="auto", segment_seconds=300, from_sec=0, to_sec=0, include_raw=false)` | Channel stats, laps, interval segments, the time series and HR. |
| `get_session_heart_rate(session_id, date?, sample_every_seconds=10)` | The HR trace, zones and avg/min/max HR. |

Interval analysis: call `get_exercise_analytics` once with defaults to get the intervals, then
again with `from_sec`/`to_sec` set to one interval and `sample_every_seconds=1`.

## Local setup

```sh
cargo install technogym-mcp      # or the installer / archive from a GitHub Release
technogym-mcp login              # asks for email and password, saves the session
```

**Claude Code:**

```sh
claude mcp add technogym -- technogym-mcp
```

**Claude Desktop** (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "technogym": { "command": "technogym-mcp" }
  }
}
```

`technogym-mcp logout` deletes the saved session.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `TECHNOGYM_EMAIL` / `TECHNOGYM_PASSWORD` | | Sign in unattended when there is no saved session, and renew an expired one. `MYWELLNESS_EMAIL` / `MYWELLNESS_PASSWORD` work too. A failing password is tried once per process, never on every call. |
| `TECHNOGYM_MCP_SESSION_DIR` | `~/.technogym-mcp/session` | Where the session is saved. |
| `TECHNOGYM_FACILITY` | first facility | Facility id or url slug, for accounts with several gyms. |
| `RUST_LOG` | `info` | Log level; logs go to stderr. |

## Hosted

`technogym-mcp serve-http` serves the same tools over streamable HTTP behind an OAuth 2.1 sign-in,
so it can be added as a custom connector in claude.ai (Settings → Connectors). Users sign in with
their mywellness email and password on the server's page; the password goes only to mywellness
and nothing is stored on the server. See [HOSTING.md](HOSTING.md).

## License

MPL-2.0. Inspired by technogym-js and health_mcp (see NOTICE); not a port.

# technogym-rs

**Rust client and MCP server for Technogym mywellness**

<sub>**Unofficial.** Not affiliated with, endorsed by, or supported by Technogym. Technogym and
mywellness are trademarks of Technogym S.p.A.</sub>

Reads your gym training history from Technogym's **mywellness** cloud: workout sessions, every
exercise with per-set reps and weight, the machine's full summary (a Skillrun's running cadence,
power, step length and ground contact time; a strength machine's estimated 1RM), per-second
treadmill and bike data, and whole-session heart rate.

Technogym's official API is partner-only. This project speaks the JSON API the end-user web app
uses, signing in with your normal email and password. It will break if Technogym changes the
login flow or its client-trust rules. Read-only: nothing here changes your account.

| Crate | What | Docs |
|---|---|---|
| [`technogym`](crates/technogym) | Async client library: sign-in, session, endpoints, and the interval / lap analysis the API leaves out. | [README](crates/technogym/README.md) · [docs.rs](https://docs.rs/technogym) |
| [`technogym-mcp`](crates/technogym-mcp) | MCP server with seven read-only tools: on stdio for Claude Code / Desktop, or hosted over HTTP with OAuth for claude.ai connectors. | [README](crates/technogym-mcp/README.md) · [HOSTING](crates/technogym-mcp/HOSTING.md) |

## Install

```sh
cargo add technogym                 # the library
cargo install technogym-mcp         # the MCP server
```

Prebuilt binaries for Linux (glibc and musl) and macOS, x86_64 and aarch64, are attached to every
[GitHub Release](https://github.com/StephanMeijer/technogym-rs/releases) with cosign signatures
and SLSA provenance, next to a shell installer. The hosted server ships as
`ghcr.io/stephanmeijer/technogym-mcp`.

## Quick start

```rust
use technogym::{Credentials, SegmentOptions, Technogym};

let tg = Technogym::builder()
    .credentials(Credentials::new("me@example.com", "secret"))
    .build();
for session in tg.get_recent_workouts(7).await? {
    println!("{} {} ({} exercises)", session.date, session.name, session.exercises.len());
    for run in session.exercises.iter().filter(|e| e.is_cardio) {
        let analyzed = tg.analyze_exercise(&run.analytics_id, SegmentOptions::default()).await?;
        for s in &analyzed.segments {
            println!("  interval {:?} at {:?}: {:?} min/km", s.interval, s.setpoint, s.pace_min_per_km);
        }
    }
}
```

```sh
technogym-mcp login                                   # once; saves the session
claude mcp add technogym -- technogym-mcp             # Claude Code
```

## Development

```sh
cp .env.example .env     # optional: TECHNOGYM_EMAIL / TECHNOGYM_PASSWORD for a real login
just                     # fmt-check, clippy -D warnings, tests, docs, cargo-machete, cargo-deny
```

Both crates share one version and release together; see [RELEASING.md](RELEASING.md).

## Credits

Inspired by [technogym-js](https://github.com/DynamicsNinja/technogym-js) and the Technogym
server in [health_mcp](https://github.com/ErikAnkerKilbergSkallevold/health_mcp), which showed
which endpoints matter and what a useful tool set looks like. This is written against the
mywellness API, not a port of either. See [NOTICE](NOTICE).

## License

[MPL-2.0](LICENSE)

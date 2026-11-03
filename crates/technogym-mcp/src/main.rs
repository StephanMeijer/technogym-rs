//! `technogym-mcp`: the MCP server on stdio (default), `login`, `logout`, or `serve-http`.

use std::sync::Arc;

use clap::{Parser, Subcommand};
use rmcp::ServiceExt as _;
use technogym::{FileSessionStore, SessionStore as _, Technogym};
use technogym_mcp::config::McpConfig;
use technogym_mcp::{FileSource, TechnogymMcp};

/// Unofficial MCP server for Technogym mywellness.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
#[allow(clippy::doc_markdown)]
enum Command {
    /// Sign in to mywellness once and save the session (email/password from the environment or the terminal).
    Login,
    /// Delete the saved session.
    Logout,
    /// Serve over streamable HTTP behind OAuth (PUBLIC_URL, SEAL_KEYS, PORT; see HOSTING.md).
    ServeHttp,
}

#[tokio::main]
async fn main() {
    // stdout carries MCP frames on stdio; everything human-readable goes to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let config = McpConfig::from_env();
    let outcome = match Cli::parse().command {
        None => serve_stdio(config).await,
        Some(Command::Login) => login(config).await,
        Some(Command::Logout) => logout(config).await,
        Some(Command::ServeHttp) => technogym_mcp::http::app::serve_from_env().await,
    };
    if let Err(e) = outcome {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn serve_stdio(config: McpConfig) -> Result<(), Box<dyn std::error::Error>> {
    let source = FileSource::new(config.session_dir, config.credentials, config.facility);
    let server = TechnogymMcp::new(Arc::new(source))
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}

async fn login(config: McpConfig) -> Result<(), Box<dyn std::error::Error>> {
    let (email, password) = if let Some(c) = config.credentials {
        (c.email, c.password)
    } else {
        eprint!("mywellness email: ");
        let mut email = String::new();
        std::io::stdin().read_line(&mut email)?;
        eprint!("mywellness password: ");
        let password = rpassword::read_password()?;
        (email.trim().to_string(), password)
    };
    if email.is_empty() || password.is_empty() {
        return Err("Email and password are required.".into());
    }
    let mut builder =
        Technogym::builder().session_store(FileSessionStore::new(&config.session_dir));
    if let Some(f) = config.facility {
        builder = builder.facility(f);
    }
    let session = builder.build().login(&email, &password).await?;
    let facilities = session
        .user
        .facilities
        .iter()
        .map(|f| if f.name.is_empty() { &f.url } else { &f.name })
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "Logged in as {} {} (facilities: {}).",
        session.user.first_name,
        session.user.last_name,
        if facilities.is_empty() {
            "none".to_string()
        } else {
            facilities
        }
    );
    eprintln!("Session saved to {}.", config.session_dir.display());
    Ok(())
}

async fn logout(config: McpConfig) -> Result<(), Box<dyn std::error::Error>> {
    FileSessionStore::new(&config.session_dir).clear().await?;
    eprintln!("Removed the session in {}.", config.session_dir.display());
    Ok(())
}

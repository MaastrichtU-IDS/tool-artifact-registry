//! `tar` — the Tool Artifact Registry binary.
//!
//! One statically linked binary: HTTP API, embedded triplestore, embedded operational
//! database, and (when built) the web UI. `docker run -e TAR_BASE_IRI=… -v tar-data:/data`
//! is a complete install (spec §10).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tar::{api, seed, state::AppState, Config};

#[derive(Parser)]
#[command(name = "tar", version, about = "Tool Artifact Registry")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server (the default).
    Serve,
    /// Load bootstrap content so a fresh install is demonstrable immediately (spec §10.7).
    Seed {
        /// Currently only `ids-examples`.
        #[arg(long, default_value = "ids-examples")]
        from: String,
        /// Also generate example runs and artifacts, including one cross-registry input.
        #[arg(long, default_value_t = true)]
        with_runs: bool,
    },
    /// Container healthcheck — the compose healthcheck runs `tar healthcheck`.
    Healthcheck,
    /// Stream every graph as N-Quads (spec §10.6).
    Dump {
        #[arg(long)]
        graph: Option<String>,
    },
    /// Restore from an N-Quads dump.
    Restore {
        #[arg(long)]
        nquads: String,
    },
    /// Print the effective configuration, with secrets redacted.
    Config,
    /// Rename this registry's records from an old base IRI to the current `TAR_BASE_IRI`.
    /// Run with the registry stopped, after a backup (design note: base IRI rebase).
    Rebase {
        /// The base the records are under now.
        #[arg(long)]
        from: String,
        /// Count what would change and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TAR_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=warn")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Healthcheck => healthcheck().await,
        Command::Serve => serve().await,
        Command::Seed { from, with_runs } => {
            let state = boot().await?;
            if from != "ids-examples" {
                anyhow::bail!("unknown seed source {from:?}; the prototype ships `ids-examples`");
            }
            let report = seed::seed_ids_examples(&state, with_runs).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Dump { graph } => {
            let state = boot().await?;
            // N-Quads either way, so whatever `dump` writes, `restore` puts back where it was.
            match graph {
                Some(g) => print!("{}", state.store.dump_graph_nquads(&g)?),
                None => print!("{}", state.store.dump_nquads(None)?),
            }
            Ok(())
        }
        Command::Restore { nquads } => {
            let state = boot().await?;
            let data = std::fs::read_to_string(&nquads).with_context(|| format!("reading {nquads}"))?;
            let n = state.store.load_nquads(&data)?;
            println!("loaded {n} quads");
            Ok(())
        }
        Command::Rebase { from, dry_run } => {
            // Not `boot()`: its vocabulary reload rewrites the bundle graphs whenever the base
            // has changed, and a dry run must write nothing. The next `serve` reloads them.
            let state = AppState::new(Config::from_env()?).await?;
            let report = tar::rebase::run(&state, &from, dry_run).await?;
            let verb = if dry_run { "would rewrite" } else { "rewrote" };
            if report.is_empty() {
                println!("nothing under {from} — already rebased, or the wrong base");
            } else {
                println!("{verb} {:>7}  statements in the local graph", report.statements);
                for (column, n) in &report.rows {
                    println!("{verb} {n:>7}  {column}");
                }
                if let Some(p) = &report.snapshot {
                    println!("the local graph as it was is in {}", p.display());
                }
            }
            Ok(())
        }
        Command::Config => {
            let c = Config::from_env()?;
            println!("base_iri              {}", c.base_iri);
            if !c.previous_base_iris.is_empty() {
                println!("previous_base_iris    {}", c.previous_base_iris.join(", "));
            }
            println!("data_dir              {}", c.data_dir);
            match &c.sparql_backend {
                Some(b) => println!("graph store           external SPARQL endpoint — {}", b.describe()),
                None => {
                    println!("graph store           embedded oxigraph at {}/graph", c.data_dir.trim_end_matches('/'))
                }
            }
            println!("listen                {}", c.listen);
            println!("public_read           {}", c.public_read);
            println!("sparql_public         {}", c.sparql_public);
            println!("shacl_validate_writes {}", c.shacl_validate_writes);
            println!("root_token            {}", if c.root_token.is_some() { "set" } else { "unset" });
            println!("static_dir            {}", c.static_dir.unwrap_or_else(|| "(none — API only)".into()));
            println!("oidc issuer           {}", c.oidc.issuer.unwrap_or_else(|| "(unset)".into()));
            println!("workload issuers      {}", c.oidc.workload_issuers.join(", "));
            println!("oidc client claim     {}", c.oidc.client_claim);
            println!("peer resolve          {} (ttl {:?})", c.peer_resolve_enabled, c.peer_resolve_ttl);
            let forge_token = if tar::domain::forge::token_for(None).is_some() { "token" } else { "anonymous" };
            println!("forge poll interval   {:?} ({forge_token})", c.forge_poll_interval);
            let rl = &c.rate_limit;
            let side = |l: Option<tar::ratelimit::Limit>| l.map_or_else(|| "off".to_string(), |l| l.to_string());
            println!("rate_limit            {}", if rl.enabled { "on" } else { "off" });
            let proxies: Vec<String> = rl.trusted_proxies.iter().map(ToString::to_string).collect();
            println!("trusted_proxies       {}", if proxies.is_empty() { "(none)".into() } else { proxies.join(", ") });
            for class in tar::ratelimit::Class::ALL {
                let l = rl.limits(class);
                println!("rate_limit.{:<11}anon {} · authed {}", class.name(), side(l.anon), side(l.authed));
            }
            println!("rate_limit.auth_fail  {}", side(rl.auth_fail));
            Ok(())
        }
    }
}

async fn boot() -> Result<Arc<AppState>> {
    let config = Config::from_env()?;
    let state = AppState::new(config).await?;
    // Idempotent: shapes and vocabulary are reloaded on every boot, which is also how graph
    // migrations are applied (spec §10.6).
    seed::load_vocab(&state)?;
    Ok(state)
}

async fn serve() -> Result<()> {
    let state = boot().await?;
    let listen = state.config.listen.clone();

    match tar::rebase::stray_base(&state) {
        Ok(Some(old)) => tracing::warn!(
            "records in the store are under {old}, not TAR_BASE_IRI {}; if the registry moved, \
             stop it and run `tar rebase --from {old}`",
            state.config.base_iri
        ),
        Ok(None) => {}
        Err(e) => tracing::warn!("could not check the store for records under another base: {e:#}"),
    }
    if state.config.root_token.is_none() {
        tracing::warn!(
            "TAR_ROOT_TOKEN is unset — no bootstrap admin exists, so nothing can be registered \
             through the API until you set one"
        );
    }
    if state.config.oidc.enabled() {
        tracing::info!(issuers = ?state.config.oidc.accepted_issuers(), "OIDC enabled: workloads may authenticate with their own tokens");
    } else {
        tracing::info!("OIDC not configured; registry API tokens only");
    }

    if state.config.peer_resolve_enabled {
        tokio::spawn(api::peers::resolver_loop(state.clone()));
    }
    tokio::spawn(tar::health::check_loop(state.clone()));
    tokio::spawn(tar::domain::forge::poll_loop(state.clone()));
    // Webhook delivery, off the request path for the same reason peer resolution is: nothing a
    // subscriber's endpoint does may be felt by the deployment that advertised.
    tokio::spawn(api::subscriptions::delivery_loop(state.clone()));
    if state.config.rate_limit.enabled {
        tokio::spawn(tar::ratelimit::retain_loop(state.clone()));
    }

    let app = tar::app(state.clone());
    let listener = tokio::net::TcpListener::bind(&listen).await.with_context(|| format!("binding {listen}"))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, base_iri = %state.config.base_iri, "tool-artifact-registry listening");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

async fn healthcheck() -> Result<()> {
    let listen = std::env::var("TAR_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let port = listen.rsplit(':').next().unwrap_or("8080");
    let url = format!("http://127.0.0.1:{port}/healthz");
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        anyhow::bail!("healthcheck failed: {}", resp.status())
    }
}

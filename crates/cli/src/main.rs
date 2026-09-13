//! `hither`: bring files here, directly, peer to peer.
//!
//!     hither scans/                # share a folder
//!     hither a.jpg b.tiff          # share a few files
//!     hither <ticket or link>      # receive
//!     hither get <ticket or link>  # same, spelled out
//!     hither id                    # your stable identity
//!     hither doctor                # can this network do direct connections?

mod ui;

use std::{path::PathBuf, str::FromStr, time::Duration};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use hither_core::{
    CancellationToken, Cancelled, DoctorOptions, Identity, ReceiveOptions, RelayMode, SendOptions,
    Sender, TicketKind, link,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "hither",
    version,
    about = "Bring files hither. Direct, verified, peer to peer.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Files or folders to share, or a ticket/link to receive.
    #[arg(value_name = "PATH|LINK")]
    items: Vec<String>,

    #[command(flatten)]
    send: SendFlags,

    #[command(flatten)]
    get: GetFlags,

    #[command(flatten)]
    common: Common,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Share files or folders (the default when paths are given).
    Send {
        /// Files or folders to share.
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        flags: SendFlags,
        #[command(flatten)]
        common: Common,
    },
    /// Bring a share hither from a ticket or link (the default when a link is given).
    #[command(visible_alias = "receive")]
    Get {
        /// The ticket or link you were sent.
        #[arg(value_name = "TICKET|LINK")]
        link: String,
        #[command(flatten)]
        flags: GetFlags,
        #[command(flatten)]
        common: Common,
    },
    /// Show (and on first use, create) your stable identity.
    Id {
        /// Print only the endpoint id.
        #[arg(long)]
        short: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Check whether this network allows direct connections and can reach a relay.
    Doctor {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Args, Debug, Clone, Default)]
struct SendFlags {
    /// Mint a shorter ticket carrying only your endpoint id. Receivers then
    /// look you up through iroh's discovery service.
    #[arg(long)]
    short: bool,

    /// Also print the ticket or link as a QR code.
    #[arg(long)]
    qr: bool,

    /// Wrap the ticket in a link: <URL>/#<ticket>.
    #[arg(long, env = "HITHER_LINK_BASE", value_name = "URL")]
    link_base: Option<String>,

    /// Share under your stable identity (see `hither id`) instead of a
    /// fresh one, so the other side can recognise you.
    #[arg(long)]
    identity: bool,
}

#[derive(Args, Debug, Clone, Default)]
struct GetFlags {
    /// Directory to save into. Defaults to the current directory.
    #[arg(short, long, value_name = "DIR")]
    out: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct Common {
    /// Relay servers: "default", "disabled", or a relay URL.
    #[arg(
        long,
        env = "HITHER_RELAY",
        default_value = "default",
        value_name = "MODE"
    )]
    relay: RelayArg,

    /// More output (-v info, -vv debug). RUST_LOG overrides this.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Debug, Clone)]
enum RelayArg {
    Default,
    Disabled,
    Custom(iroh::RelayUrl),
}

impl FromStr for RelayArg {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "default" => Ok(Self::Default),
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            url => Ok(Self::Custom(
                iroh::RelayUrl::from_str(url)
                    .context("relay must be default, disabled, or a URL")?,
            )),
        }
    }
}

impl From<RelayArg> for RelayMode {
    fn from(value: RelayArg) -> Self {
        match value {
            RelayArg::Default => RelayMode::Default,
            RelayArg::Disabled => RelayMode::Disabled,
            RelayArg::Custom(url) => RelayMode::Custom(url.into()),
        }
    }
}

enum Action {
    Send(Vec<PathBuf>, SendFlags, Common),
    Get(String, GetFlags, Common),
    Id(bool, Common),
    Doctor(bool, Common),
}

fn decide(cli: Cli) -> Action {
    match cli.command {
        Some(Command::Send {
            paths,
            flags,
            common,
        }) => Action::Send(paths, flags, common),
        Some(Command::Get {
            link,
            flags,
            common,
        }) => Action::Get(link, flags, common),
        Some(Command::Id { short, common }) => Action::Id(short, common),
        Some(Command::Doctor { json, common }) => Action::Doctor(json, common),
        None => {
            if cli.items.is_empty() {
                Cli::command().print_help().ok();
                std::process::exit(2);
            }
            // A single argument that is not a path but parses as a ticket or
            // link means "receive".
            if cli.items.len() == 1
                && !std::path::Path::new(&cli.items[0]).exists()
                && link::looks_like_ticket(&cli.items[0])
            {
                return Action::Get(cli.items.into_iter().next().unwrap(), cli.get, cli.common);
            }
            let paths = cli.items.into_iter().map(PathBuf::from).collect();
            Action::Send(paths, cli.send, cli.common)
        }
    }
}

fn init_tracing(verbose: u8) {
    let default = match verbose {
        0 => "error",
        1 => "hither=info,hither_core=info,iroh=warn,iroh_blobs=warn",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() {
    let action = decide(Cli::parse());
    let result = match action {
        Action::Send(paths, flags, common) => {
            init_tracing(common.verbose);
            run_send(paths, flags, common).await
        }
        Action::Get(link, flags, common) => {
            init_tracing(common.verbose);
            run_get(link, flags, common).await
        }
        Action::Id(short, common) => {
            init_tracing(common.verbose);
            run_id(short)
        }
        Action::Doctor(json, common) => {
            init_tracing(common.verbose);
            run_doctor(json, common).await
        }
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{} {e:#}", console::style("error:").red().bold());
            std::process::exit(1);
        }
    }
}

async fn run_send(paths: Vec<PathBuf>, flags: SendFlags, common: Common) -> Result<i32> {
    let secret_key = if flags.identity {
        Some(Identity::load_default()?.secret_key().clone())
    } else {
        None
    };
    let (tx, rx) = hither_core::channel();
    let ui = tokio::spawn(ui::render_send(rx, flags.qr, common.verbose > 0));
    let opts = SendOptions {
        ticket_kind: if flags.short {
            TicketKind::Short
        } else {
            TicketKind::Full
        },
        relay: common.relay.into(),
        link_base: flags.link_base,
        secret_key,
        ..SendOptions::default()
    };
    let sender = match Sender::start(&paths, opts, tx.clone()).await {
        Ok(sender) => sender,
        Err(e) => {
            drop(tx);
            ui.await.ok();
            return Err(e);
        }
    };
    tokio::signal::ctrl_c().await?;
    sender.shutdown().await?;
    drop(tx);
    ui.await.ok();
    eprintln!("Stopped sharing.");
    Ok(0)
}

async fn run_get(link: String, flags: GetFlags, common: Common) -> Result<i32> {
    let ticket = link::parse(&link)?;
    let out_dir = flags.out.unwrap_or_else(|| PathBuf::from("."));
    let (tx, rx) = hither_core::channel();
    let ui = tokio::spawn(ui::render_get(rx));
    let cancel = CancellationToken::new();
    let cancel_on_ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel_on_ctrl_c.cancel();
        }
    });
    let result = hither_core::receive(
        ticket,
        ReceiveOptions {
            out_dir,
            relay: common.relay.into(),
            secret_key: None,
        },
        tx,
        cancel,
    )
    .await;
    ui.await.ok();
    match result {
        Ok(_) => Ok(0),
        Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
            eprintln!("\nStopped. Verified data was kept; run the same command again to resume.");
            Ok(130)
        }
        Err(e) => Err(e),
    }
}

fn run_id(short: bool) -> Result<i32> {
    let id = Identity::load_default()?;
    if short {
        println!("{}", id.endpoint_id());
        return Ok(0);
    }
    if id.was_created() {
        eprintln!("{}", console::style("Created a new identity.").green());
    }
    println!(
        "{:<12} {}",
        console::style("endpoint id").dim(),
        id.endpoint_id()
    );
    match id.path() {
        Some(p) => println!("{:<12} {}", console::style("stored at").dim(), p.display()),
        None => println!(
            "{:<12} {}",
            console::style("source").dim(),
            format!("{} environment variable", hither_core::identity::ENV_SECRET)
        ),
    }
    Ok(0)
}

async fn run_doctor(json: bool, common: Common) -> Result<i32> {
    let report = hither_core::diagnose(DoctorOptions {
        relay: common.relay.into(),
        timeout: Duration::from_secs(12),
    })
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        ui::render_doctor(&report);
    }
    Ok(match report.verdict {
        hither_core::Verdict::DirectLikely => 0,
        hither_core::Verdict::RelayOnly => 2,
        hither_core::Verdict::Offline => 3,
    })
}

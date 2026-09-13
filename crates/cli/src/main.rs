//! `hither`: bring files here, directly, peer to peer.
//!
//!     hither scans/                    # share a folder
//!     hither a.jpg b.tiff              # share a few files
//!     hither <ticket or link>          # receive
//!     hither scans/ --code             # also print four words to say aloud
//!     hither able-cactus-river-mouse    # receive by code
//!     hither inbox                     # open your inbox and print its link
//!     hither to <inbox link> scans/    # offer files to someone's inbox
//!     hither friends add sam <link>    # save an inbox under a name
//!     hither sam scans/                # offer files to a saved friend
//!     hither id                        # your stable identity
//!     hither id export                 # the secret behind it, to move machines
//!     hither doctor                    # can this network do direct connections?
//!     hither upgrade                   # replace this binary with the newest release

mod ui;

use std::{collections::BTreeSet, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use hither_core::{
    AcceptPolicy, CancellationToken, Cancelled, Code, DoctorOptions, EndpointId, Friends, Identity,
    Inbox, InboxOptions, NetOptions, ReceiveOptions, RelayMode, SendOptions, Sender, TicketKind,
    friends,
    link::{self, Link},
};
use tracing_subscriber::EnvFilter;

/// Where printed links point. The page reads the ticket from the fragment
/// and walks the other person through installing and running hither.
const DEFAULT_LINK_BASE: &str = "https://alduncanson.github.io/hither/";

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

    /// Files or folders to share, a ticket/link to receive, or an inbox
    /// link followed by files to offer.
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
    /// Open your inbox: print a stable link people can drop files into.
    Inbox {
        #[command(flatten)]
        flags: InboxFlags,
        #[command(flatten)]
        common: Common,
    },
    /// Offer files to someone's inbox and stay until they have them.
    To {
        /// Their inbox ticket or link, or a saved friend's name.
        #[arg(value_name = "INBOX|NAME")]
        inbox: String,
        /// Files or folders to offer.
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        flags: ToFlags,
        #[command(flatten)]
        common: Common,
    },
    /// Save inbox links under short names: `hither to sam photos/`.
    Friends {
        #[command(subcommand)]
        cmd: FriendsCmd,
    },
    /// Show (and on first use, create) your stable identity.
    Id {
        #[command(subcommand)]
        cmd: Option<IdCmd>,
        /// Print only the endpoint id.
        #[arg(long)]
        short: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Replace this binary with the newest release.
    Upgrade,
    /// Check whether this network allows direct connections and can reach a relay.
    Doctor {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Subcommand, Debug)]
enum IdCmd {
    /// Print the secret behind your identity, to move it to another machine.
    /// Anyone holding it can act as you: store it like a password.
    Export {
        /// Print 64 hex characters instead of 24 words.
        #[arg(long)]
        hex: bool,
    },
    /// Install a secret exported elsewhere as this machine's identity.
    Import {
        /// The 24 words or 64 hex characters from `hither id export`.
        #[arg(required = true, value_name = "WORDS|HEX")]
        secret: Vec<String>,
        /// The inbox token printed by `export`, so old inbox links keep working.
        #[arg(long, value_name = "HEX")]
        token: Option<String>,
        /// Replace a different identity already on this machine.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum FriendsCmd {
    /// Remember an inbox link as a name.
    Add {
        /// Lowercase letters, digits, '-' or '_'.
        name: String,
        /// Their inbox ticket or link.
        link: String,
    },
    /// List saved friends.
    List,
    /// Forget a saved friend.
    Remove { name: String },
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

    /// Also mint four words the other side can type instead of the link:
    /// `hither able-cactus-river-mouse`. Works while this command runs.
    #[arg(long)]
    code: bool,

    /// Base of the printed link, <URL>#<ticket>. Pass an empty string to
    /// print only the ticket.
    #[arg(long, env = "HITHER_LINK_BASE", value_name = "URL", default_value = DEFAULT_LINK_BASE)]
    link_base: String,

    /// Share under your stable identity (see `hither id`) instead of a
    /// fresh one, so the other side can recognise you.
    #[arg(long)]
    identity: bool,

    /// When offering to an inbox: a name to show the other side.
    #[arg(long = "as", value_name = "NAME")]
    label: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
struct GetFlags {
    /// Directory to save into. Defaults to your Downloads folder.
    #[arg(short, long, value_name = "DIR")]
    out: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, Default)]
struct InboxFlags {
    /// Directory offers are saved under. Defaults to your Downloads folder.
    #[arg(short, long, value_name = "DIR")]
    dir: Option<PathBuf>,

    /// Accept every offer that carries your link, without asking.
    #[arg(long)]
    accept_all: bool,

    /// Accept offers from these endpoint ids without asking (repeatable).
    #[arg(long, value_name = "ENDPOINT_ID")]
    accept_from: Vec<String>,

    /// Base of the printed link, <URL>#<ticket>. Pass an empty string to
    /// print only the ticket.
    #[arg(long, env = "HITHER_LINK_BASE", value_name = "URL", default_value = DEFAULT_LINK_BASE)]
    link_base: String,

    /// Also print the link as a QR code.
    #[arg(long)]
    qr: bool,

    /// Replace the inbox token, which invalidates every link handed out so far.
    #[arg(long)]
    rotate: bool,
}

#[derive(Args, Debug, Clone, Default)]
struct ToFlags {
    /// A name to show the other side, e.g. --as "Sam".
    #[arg(long = "as", value_name = "NAME")]
    label: Option<String>,

    /// Offer under your stable identity (see `hither id`).
    #[arg(long)]
    identity: bool,
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
    Inbox(InboxFlags, Common),
    To(String, Vec<PathBuf>, ToFlags, Common),
    Friends(FriendsCmd),
    Id(Option<IdCmd>, bool, Common),
    Upgrade,
    Doctor(bool, Common),
}

fn decide(cli: Cli) -> Result<Action> {
    Ok(match cli.command {
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
        Some(Command::Inbox { flags, common }) => Action::Inbox(flags, common),
        Some(Command::To {
            inbox,
            paths,
            flags,
            common,
        }) => Action::To(inbox, paths, flags, common),
        Some(Command::Friends { cmd }) => Action::Friends(cmd),
        Some(Command::Id { cmd, short, common }) => Action::Id(cmd, short, common),
        Some(Command::Upgrade) => Action::Upgrade,
        Some(Command::Doctor { json, common }) => Action::Doctor(json, common),
        None => {
            if cli.items.is_empty() {
                Cli::command().print_help().ok();
                std::process::exit(2);
            }
            // A first argument that is not a path but parses as a ticket or
            // link decides the mode: a share link means receive, an inbox
            // link followed by paths means offer.
            let first = &cli.items[0];
            let first_is_path = std::path::Path::new(first).exists();
            // Four words, as one dashed token or four arguments, none a path.
            let joined = cli.items.join(" ");
            if !cli.items.iter().any(|i| std::path::Path::new(i).exists())
                && Code::looks_like(&joined)
            {
                return Ok(Action::Get(joined, cli.get, cli.common));
            }
            // `hither sam photos/`: a saved friend's name followed by paths.
            if !first_is_path && cli.items.len() >= 2 && friends::is_friend(first) {
                return Ok(Action::To(
                    first.clone(),
                    cli.items[1..].iter().map(PathBuf::from).collect(),
                    ToFlags {
                        label: cli.send.label.clone(),
                        identity: cli.send.identity,
                    },
                    cli.common,
                ));
            }
            if !first_is_path && cli.items.len() == 1 && friends::is_friend(first) {
                bail!(
                    "{first} is a saved friend. Offer files with:\n  hither {first} <files or folders>"
                );
            }
            match (first_is_path, link::parse_any(first)) {
                (false, Ok(Link::Share(_))) if cli.items.len() == 1 => {
                    Action::Get(first.clone(), cli.get, cli.common)
                }
                (false, Ok(Link::Share(_))) => {
                    bail!("a share link takes no other arguments; use --out to choose a folder")
                }
                (false, Ok(Link::Inbox(_))) if cli.items.len() == 1 => {
                    bail!(
                        "that is an inbox link. Offer files to it with:\n  hither to {first} <files or folders>"
                    )
                }
                (false, Ok(Link::Inbox(_))) => Action::To(
                    first.clone(),
                    cli.items[1..].iter().map(PathBuf::from).collect(),
                    ToFlags {
                        label: cli.send.label.clone(),
                        identity: cli.send.identity,
                    },
                    cli.common,
                ),
                _ => Action::Send(
                    cli.items.into_iter().map(PathBuf::from).collect(),
                    cli.send,
                    cli.common,
                ),
            }
        }
    })
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
    let action = match decide(Cli::parse()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{} {e:#}", console::style("error:").red().bold());
            std::process::exit(2);
        }
    };
    let result = match action {
        Action::Send(paths, flags, common) => {
            init_tracing(common.verbose);
            run_send(paths, flags, common).await
        }
        Action::Get(link, flags, common) => {
            init_tracing(common.verbose);
            run_get(link, flags, common).await
        }
        Action::Inbox(flags, common) => {
            init_tracing(common.verbose);
            run_inbox(flags, common).await
        }
        Action::To(inbox, paths, flags, common) => {
            init_tracing(common.verbose);
            run_to(inbox, paths, flags, common).await
        }
        Action::Friends(cmd) => run_friends(cmd),
        Action::Id(cmd, short, common) => {
            init_tracing(common.verbose);
            match cmd {
                None => run_id(short),
                Some(IdCmd::Export { hex }) => run_id_export(hex),
                Some(IdCmd::Import {
                    secret,
                    token,
                    force,
                }) => run_id_import(&secret.join(" "), token, force),
            }
        }
        Action::Upgrade => run_upgrade(),
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

/// Network options from the shared flags, plus an optional identity key.
fn net(common: &Common, secret_key: Option<hither_core::SecretKey>) -> NetOptions {
    NetOptions {
        relay: common.relay.clone().into(),
        secret_key,
        ..NetOptions::default()
    }
}

/// An empty `--link-base` means "no link, just the ticket".
fn link_base(flag: &str) -> Option<String> {
    let trimmed = flag.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Where received files go unless told otherwise: the OS Downloads folder,
/// like a browser, so nothing depends on which directory a command was run
/// from. Falls back to the current directory if there is no such folder.
fn downloads_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn identity_key(use_identity: bool) -> Result<Option<hither_core::SecretKey>> {
    Ok(if use_identity {
        Some(Identity::load_default()?.secret_key().clone())
    } else {
        None
    })
}

/// After the first Ctrl-C has been handled gracefully, a second one must
/// always end the process, whatever a shutdown is waiting on.
fn exit_on_next_ctrl_c() {
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!();
            std::process::exit(130);
        }
    });
}

/// Wait for a renderer to drain, but never hang the process on it.
async fn finish_ui(ui: tokio::task::JoinHandle<()>) {
    let _ = tokio::time::timeout(Duration::from_secs(2), ui).await;
}

fn ctrl_c_token() -> CancellationToken {
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c.cancel();
            exit_on_next_ctrl_c();
        }
    });
    cancel
}

async fn run_send(paths: Vec<PathBuf>, flags: SendFlags, common: Common) -> Result<i32> {
    let secret_key = identity_key(flags.identity)?;
    let (tx, rx) = hither_core::channel();
    let ui = tokio::spawn(ui::render_send(rx, flags.qr, common.verbose > 0));
    let opts = SendOptions {
        net: net(&common, secret_key),
        ticket_kind: if flags.short {
            TicketKind::Short
        } else {
            TicketKind::Full
        },
        link_base: link_base(&flags.link_base),
        code: flags.code,
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
    exit_on_next_ctrl_c();
    sender.shutdown().await?;
    drop(tx);
    finish_ui(ui).await;
    eprintln!("Stopped sharing.");
    Ok(0)
}

async fn run_get(link: String, flags: GetFlags, common: Common) -> Result<i32> {
    let ticket = if let Ok(code) = Code::parse(&link) {
        eprintln!("{}", console::style(format!("Looking up {code}…")).dim());
        let payload = hither_core::code::redeem(&code, &net(&common, None)).await?;
        match link::parse_any(&payload)? {
            Link::Share(t) => t,
            Link::Inbox(_) => {
                bail!("that code belongs to an inbox; inbox codes are not supported yet")
            }
        }
    } else {
        link::parse(&link)?
    };
    let out_dir = flags.out.unwrap_or_else(downloads_dir);
    let (tx, rx) = hither_core::channel();
    let ui = tokio::spawn(ui::render_get(rx));
    let cancel = ctrl_c_token();
    let result = hither_core::receive(
        ticket,
        ReceiveOptions {
            out_dir,
            net: net(&common, None),
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

async fn run_inbox(flags: InboxFlags, common: Common) -> Result<i32> {
    let identity = Identity::load_default()?;
    let token = hither_core::inbox::load_or_create_token(flags.rotate)?;
    let policy = if flags.accept_all {
        AcceptPolicy::AcceptAll
    } else if !flags.accept_from.is_empty() {
        let mut set = BTreeSet::new();
        for id in &flags.accept_from {
            set.insert(
                EndpointId::from_str(id).with_context(|| format!("{id} is not an endpoint id"))?,
            );
        }
        AcceptPolicy::AcceptFrom(set)
    } else {
        AcceptPolicy::Ask
    };
    let policy_note = match &policy {
        AcceptPolicy::Ask => "You will be asked before anything is saved.",
        AcceptPolicy::AcceptAll => "Offers are accepted automatically.",
        AcceptPolicy::AcceptFrom(_) => {
            "Listed senders are accepted automatically; anyone else asks."
        }
    };
    let (tx, rx) = hither_core::channel();
    let inbox = Inbox::open(
        &identity,
        token,
        InboxOptions {
            dir: flags.dir.unwrap_or_else(downloads_dir),
            net: net(&common, None),
            policy,
            link_base: link_base(&flags.link_base),
        },
        tx.clone(),
    )
    .await?;
    let ui = tokio::spawn(ui::render_inbox(
        rx,
        inbox.decider(),
        flags.qr,
        common.verbose > 0,
        policy_note,
    ));
    if flags.rotate {
        eprintln!(
            "{}",
            console::style("Inbox link rotated; earlier links no longer work.").yellow()
        );
    }
    tokio::signal::ctrl_c().await?;
    exit_on_next_ctrl_c();
    inbox.shutdown().await?;
    drop(tx);
    finish_ui(ui).await;
    eprintln!("Inbox closed.");
    Ok(0)
}

async fn run_to(inbox: String, paths: Vec<PathBuf>, flags: ToFlags, common: Common) -> Result<i32> {
    let inbox = friends::resolve_inbox(&inbox)?;
    let secret_key = identity_key(flags.identity)?;
    let (tx, rx) = hither_core::channel();
    let ui = tokio::spawn(ui::render_to(rx, common.verbose > 0));
    let cancel = ctrl_c_token();
    let opts = SendOptions {
        net: net(&common, secret_key),
        ..SendOptions::default()
    };
    let result = hither_core::send_to(&inbox, &paths, flags.label, opts, tx, cancel).await;
    finish_ui(ui).await;
    match result {
        Ok(_) => Ok(0),
        Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
            eprintln!("\nStopped before the inbox had everything.");
            Ok(130)
        }
        Err(e) => Err(e),
    }
}

fn run_friends(cmd: FriendsCmd) -> Result<i32> {
    let mut book = Friends::load()?;
    match cmd {
        FriendsCmd::Add { name, link } => {
            let ticket = link::parse_inbox(&link)?;
            book.add(&name, &ticket)?;
            book.save()?;
            println!(
                "Saved {} ({}). Now: {}",
                console::style(&name).bold(),
                console::style(ticket.endpoint_id().fmt_short()).dim(),
                console::style(format!("hither {name} <files>")).green()
            );
        }
        FriendsCmd::List => {
            if book.inboxes.is_empty() {
                println!("No friends saved yet. `hither friends add <name> <inbox link>`");
            }
            for (name, ticket) in &book.inboxes {
                let short = hither_core::InboxTicket::from_str(ticket)
                    .map(|t| t.endpoint_id().fmt_short().to_string())
                    .unwrap_or_else(|_| "invalid".into());
                println!(
                    "{:<20} {}",
                    console::style(name).bold(),
                    console::style(short).dim()
                );
            }
        }
        FriendsCmd::Remove { name } => {
            if book.remove(&name) {
                book.save()?;
                println!("Forgot {name}.");
            } else {
                println!("No friend named {name}.");
            }
        }
    }
    Ok(0)
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
            "{:<12} {} environment variable",
            console::style("source").dim(),
            hither_core::identity::ENV_SECRET
        ),
    }
    Ok(0)
}

fn run_id_export(hex: bool) -> Result<i32> {
    let id = Identity::load_default()?;
    eprintln!(
        "{}",
        console::style(
            "This is your private key. Anyone holding it can act as you. Store it like a password."
        )
        .yellow()
    );
    println!("{}", if hex { id.to_hex() } else { id.to_words() });
    if let Ok(path) = hither_core::inbox::token_path()
        && let Ok(token) = std::fs::read_to_string(&path)
    {
        eprintln!(
            "{} {}",
            console::style("inbox token (pass to import with --token):").dim(),
            token.trim()
        );
    }
    eprintln!(
        "{} {}",
        console::style("endpoint id").dim(),
        id.endpoint_id()
    );
    Ok(0)
}

fn run_id_import(secret: &str, token: Option<String>, force: bool) -> Result<i32> {
    let key = Identity::parse_secret(secret)?;
    let path = hither_core::identity::default_path()?;
    let id = Identity::import(&path, key, force)?;
    println!(
        "{:<12} {}",
        console::style("endpoint id").dim(),
        id.endpoint_id()
    );
    println!(
        "{:<12} {}",
        console::style("stored at").dim(),
        path.display()
    );
    if let Some(token) = token {
        let bytes = data_encoding::HEXLOWER
            .decode(token.trim().as_bytes())
            .context("the token is not valid hex")?;
        let bytes: [u8; 16] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("the token must be 32 hex characters"))?;
        hither_core::inbox::save_token(bytes)?;
        println!(
            "{:<12} restored; existing inbox links keep working",
            console::style("inbox token").dim()
        );
    }
    Ok(0)
}

/// Re-run the published installer against the folder this binary lives in.
fn run_upgrade() -> Result<i32> {
    let exe = std::env::current_exe().context("cannot tell where hither is installed")?;
    let dir = exe
        .parent()
        .context("cannot tell where hither is installed")?
        .to_path_buf();
    if std::fs::metadata(&dir)
        .map(|m| m.permissions().readonly())
        .unwrap_or(true)
    {
        bail!(
            "{} is not writable. Re-run the installer with HITHER_INSTALL_DIR set to a folder you own",
            dir.display()
        );
    }
    eprintln!(
        "{} {} in {}",
        console::style("current").dim(),
        env!("CARGO_PKG_VERSION"),
        dir.display()
    );
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("curl -fsSL https://alduncanson.github.io/hither/install.sh | sh")
        .env("HITHER_INSTALL_DIR", &dir)
        .status()
        .context("could not run the installer")?;
    if !status.success() {
        bail!("the installer did not finish");
    }
    Ok(0)
}

async fn run_doctor(json: bool, common: Common) -> Result<i32> {
    let report = hither_core::diagnose(DoctorOptions {
        net: net(&common, None),
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

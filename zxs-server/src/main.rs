#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!("zxs-server requires x86_64 or aarch64 Linux");

mod api;
mod domain;
mod service;
mod store;

use std::{
    fs,
    io::{self, BufRead, IsTerminal, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use tracing_subscriber::util::SubscriberInitExt;
use zeroize::Zeroizing;

use domain::{ContainerId, Error, FolderPath, Result, check_address, invalid};
use service::State;
use store::{Config, Layout, Store};

#[derive(Parser)]
#[command(about = "ZXS LAN file and version storage")]
struct Args {
    #[arg(long, global = true, env = "ZXS_DATA_DIR", default_value = "zxs-data")]
    data_dir: PathBuf,
    #[arg(long, global = true, env = "ZXS_RECYCLE_DIR")]
    recycle_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Serve {
        #[arg(long)]
        allow_public_http: bool,
    },
    ResetPassword,
    Import {
        source: PathBuf,
        #[arg(long, default_value = "/")]
        folder: String,
    },
    Prune {
        #[arg(long)]
        keep_last: usize,
        #[arg(long)]
        container: Option<String>,
    },
    Verify,
    RebuildIndex,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .finish()
        .init();
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zxs: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<()> {
    if matches!(args.command, Command::Init) {
        return init(&args.data_dir, args.recycle_dir.as_deref());
    }
    let (store, mut config, lock) = configured(&args.data_dir, args.recycle_dir.as_deref())?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .map_err(|error| Error::Config(error.to_string()))?;
    if matches!(args.command, Command::ResetPassword) {
        config.password_hash = service::password_hash(&password()?)?;
        config.session_key = service::session_key()?;
        store.save_config(&config)?;
    }
    let rebuild = matches!(args.command, Command::RebuildIndex);
    let state = State::open(store, config, lock, rebuild)?;
    match args.command {
        Command::Serve { allow_public_http } => run_online(state, allow_public_http, threads),
        Command::ResetPassword => {
            println!("Password reset");
            Ok(())
        }
        Command::Import { source, folder } => {
            println!(
                "Imported {} containers",
                state.import(&source, &FolderPath::parse(folder)?)?
            );
            Ok(())
        }
        Command::Prune {
            keep_last,
            container,
        } => {
            println!(
                "Pruned {} versions",
                state.prune(keep_last, container.map(ContainerId::parse).transpose()?)?
            );
            Ok(())
        }
        Command::Verify => {
            println!("Verified {} versions", state.verify()?);
            Ok(())
        }
        Command::RebuildIndex => {
            println!("Index rebuilt");
            Ok(())
        }
        Command::Init => Err(Error::Recovery),
    }
}

fn run_online(state: State, allow_public_http: bool, threads: usize) -> Result<()> {
    let listen = state.config().listen;
    check_address(listen.ip(), allow_public_http)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(threads)
        .max_blocking_threads(32)
        .build()?
        .block_on(api::serve(state, listen, shutdown()))
}

fn init(data: &Path, recycle: Option<&Path>) -> Result<()> {
    let listen = SocketAddr::from(([127, 0, 0, 1], 7738));
    let recycle = recycle.ok_or_else(|| invalid("init requires --recycle-dir"))?;
    let password_hash = service::password_hash(&password()?)?;
    fs::create_dir_all(data)?;
    fs::create_dir_all(recycle)?;
    let paths = Layout {
        data: data.canonicalize()?,
        recycle: recycle.canonicalize()?,
    };
    disjoint(&paths)?;
    let store = Store { paths };
    store.prepare()?;
    let lock = store.lock()?;
    if store.paths.config().try_exists()? {
        return Err(Error::Conflict(
            "storage has already been initialized".into(),
        ));
    }
    let config = Config {
        listen,
        recycle_dir: store.paths.recycle.clone(),
        password_hash,
        session_key: service::session_key()?,
    };
    store.save_config(&config)?;
    State::open(store, config, lock, false)?;
    println!("Storage initialized");
    Ok(())
}

fn configured(data: &Path, recycle: Option<&Path>) -> Result<(Store, Config, fs::File)> {
    let mut store = Store {
        paths: Layout {
            data: data.canonicalize()?,
            recycle: PathBuf::new(),
        },
    };
    let lock = store.lock()?;
    let config = store.config()?;
    let root = config.recycle_dir.canonicalize()?;
    if let Some(path) = recycle
        && path.canonicalize()? != root
    {
        return Err(invalid(
            "recycle directory differs from the initialized binding",
        ));
    }
    store.paths.recycle = root;
    disjoint(&store.paths)?;
    Ok((store, config, lock))
}

fn disjoint(paths: &Layout) -> Result<()> {
    if paths.data.starts_with(&paths.recycle) || paths.recycle.starts_with(&paths.data) {
        return Err(invalid("data and recycle directories must be disjoint"));
    }
    Ok(())
}

fn password() -> Result<Zeroizing<String>> {
    if let Ok(password) = std::env::var("ZXS_PASSWORD") {
        return Ok(Zeroizing::new(password));
    }
    if io::stdin().is_terminal() {
        return Err(invalid(
            "provide the password through ZXS_PASSWORD or piped standard input",
        ));
    }
    let mut password = Zeroizing::new(String::new());
    io::stdin().lock().take(1027).read_line(&mut password)?;
    if password.ends_with('\n') {
        password.pop();
    }
    if password.ends_with('\r') {
        password.pop();
    }
    Ok(password)
}

async fn shutdown() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut sigterm) => {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = sigterm.recv() => {} }
        }
        Err(error) => {
            tracing::warn!(%error, "SIGTERM handler unavailable");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

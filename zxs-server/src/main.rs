#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!("zxs-server requires x86_64 or aarch64 Linux");

mod api;
mod domain;
mod service;
mod store;
#[cfg(test)]
mod test_support;

use std::{
    fs,
    io::{self, BufRead, IsTerminal, Read},
    net::SocketAddr,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use tracing_subscriber::util::SubscriberInitExt;
use zeroize::Zeroizing;

use domain::{ContainerId, Error, FolderPath, METADATA_LIMIT, Result, invalid};
use service::State;
use store::{Config, Layout, Store};

#[derive(Parser)]
#[command(version, about = "ZXS LAN file and version storage")]
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
        listen: Option<SocketAddr>,
    },
    SyncDisks,
    ResetPassword,
    Import {
        source: PathBuf,
        #[arg(long)]
        folder: FolderPath,
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
    let verify = matches!(args.command, Command::Verify);
    let (mut store, mut config, lock) = configured(&args.data_dir)?;
    if matches!(args.command, Command::ResetPassword) {
        let _lock = lock;
        config.password_hash = service::password_hash(&password()?)?;
        store.save_config(&config)?;
        println!("Password reset");
        return Ok(());
    }
    bind_recycle(&mut store, &config, args.recycle_dir.as_deref())?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .map_err(|error| Error::Config(error.to_string()))?;
    if verify {
        store.bind_online(&config)?;
        println!("Verified {} versions", service::verify(&store)?);
        return Ok(());
    }
    let open = |rebuild| State::open(store, config, lock, rebuild);
    match args.command {
        Command::Serve { listen } => run_online(open(true)?, listen, threads),
        Command::SyncDisks => {
            if open(false)?.sync_disks()? {
                println!("Disk registrations updated");
            } else {
                println!("No new disks to register");
            }
            Ok(())
        }
        Command::Import { source, folder } => {
            let state = open(true)?;
            println!("Imported {} containers", state.import(&source, &folder)?);
            Ok(())
        }
        Command::Prune {
            keep_last,
            container,
        } => {
            let id = container.map(ContainerId::parse).transpose()?;
            let keep = NonZeroUsize::new(keep_last)
                .ok_or_else(|| invalid("keep-last must be at least 1"))?;
            let state = open(true)?;
            println!("Pruned {} versions", state.prune(keep, id)?);
            Ok(())
        }
        Command::RebuildIndex => {
            let _state = open(true)?;
            println!("Index rebuilt");
            Ok(())
        }
        Command::Init | Command::Verify | Command::ResetPassword => Err(Error::Recovery),
    }
}

fn run_online(state: State, listen: Option<SocketAddr>, threads: usize) -> Result<()> {
    let listen = listen.unwrap_or(state.config().listen);
    let state = Arc::new(state);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(threads)
        .max_blocking_threads(32)
        .build()?;
    let result = runtime.block_on(api::serve(Arc::clone(&state), listen, shutdown()));
    // Keep the disk lock while the runtime waits for blocking I/O and staging cleanup.
    drop(runtime);
    result
}

fn init(data: &Path, recycle: Option<&Path>) -> Result<()> {
    let listen = SocketAddr::from(([0, 0, 0, 0], 7738));
    let recycle = recycle.ok_or_else(|| invalid("init requires --recycle-dir"))?;
    let password_hash = service::password_hash(&password()?)?;
    fs::create_dir_all(data)?;
    fs::create_dir_all(recycle)?;
    let paths = Layout::new(data.canonicalize()?, recycle.canonicalize()?);
    disjoint(&paths)?;
    let store = Store { paths };
    fs::create_dir_all(store.paths.system())?;
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
        data_disks: Vec::new(),
        recycle_disks: Vec::new(),
    };
    State::initialize(store, config, lock)?;
    println!("Storage initialized");
    Ok(())
}

fn configured(data: &Path) -> Result<(Store, Config, fs::File)> {
    let store = Store {
        paths: Layout::new(data.canonicalize()?, PathBuf::new()),
    };
    let lock = store.lock()?;
    let config = store.config()?;
    Ok((store, config, lock))
}

fn bind_recycle(store: &mut Store, config: &Config, recycle: Option<&Path>) -> Result<()> {
    let root = mount_root(&config.recycle_dir)?;
    if let Some(path) = recycle
        && mount_root(path)? != root
    {
        return Err(invalid(
            "recycle directory differs from the initialized binding",
        ));
    }
    store.paths.recycle = root;
    disjoint(&store.paths)
}

fn mount_root(path: &Path) -> std::io::Result<PathBuf> {
    path.canonicalize().or_else(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            std::path::absolute(path)
        } else {
            Err(error)
        }
    })
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
    if io::stdin()
        .lock()
        .take(METADATA_LIMIT as u64 + 3)
        .read_line(&mut password)?
        == 0
    {
        return Err(invalid("provide the password on standard input"));
    }
    if password.ends_with('\n') {
        password.pop();
    }
    if password.ends_with('\r') {
        password.pop();
    }
    if password.len() > METADATA_LIMIT {
        return Err(invalid("password metadata exceeds 1 MiB"));
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

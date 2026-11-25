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
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use tracing_subscriber::util::SubscriberInitExt;
use zeroize::Zeroizing;

use domain::{ContainerId, Error, FolderPath, METADATA_LIMIT, Result, invalid};
use service::State;
use store::{Config, DATA_LAYOUT, DiskKind, Layout, Store};

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
    let (mut store, mut config, lock) =
        configured(&args.data_dir, args.recycle_dir.as_deref(), verify)?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .map_err(|error| Error::Config(error.to_string()))?;
    if verify {
        println!("Verified {} versions", service::verify(&store)?);
        return Ok(());
    }
    let sync = matches!(args.command, Command::SyncDisks);
    if sync {
        config.data_disks = store.register_disks(DiskKind::Data, &config.data_disks)?;
        config.recycle_disks = store.register_disks(DiskKind::Recycle, &config.recycle_disks)?;
        store.bind(&config);
        store.validate_disks(false)?;
        store.discard_index()?;
        store.save_config(&config)?;
    }
    let rebuild = sync || matches!(args.command, Command::RebuildIndex);
    let state = State::open(store, config, lock, rebuild)?;
    match args.command {
        Command::Serve { listen } => run_online(state, listen, threads),
        Command::ResetPassword => {
            state.reset_password(&password()?)?;
            println!("Password reset");
            Ok(())
        }
        Command::Import { source, folder } => {
            println!("Imported {} containers", state.import(&source, &folder)?);
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
        Command::SyncDisks => {
            println!("Disk registrations updated");
            Ok(())
        }
        Command::RebuildIndex => {
            println!("Index rebuilt");
            Ok(())
        }
        Command::Init | Command::Verify => Err(Error::Recovery),
    }
}

fn run_online(state: State, listen: Option<SocketAddr>, threads: usize) -> Result<()> {
    let listen = listen.unwrap_or(state.config().listen);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(threads)
        .max_blocking_threads(32)
        .build()?
        .block_on(api::serve(state, listen, shutdown()))
}

fn init(data: &Path, recycle: Option<&Path>) -> Result<()> {
    let listen = SocketAddr::from(([0, 0, 0, 0], 7738));
    let recycle = recycle.ok_or_else(|| invalid("init requires --recycle-dir"))?;
    let password_hash = service::password_hash(&password()?)?;
    fs::create_dir_all(data)?;
    fs::create_dir_all(recycle)?;
    let paths = Layout::new(data.canonicalize()?, recycle.canonicalize()?);
    disjoint(&paths)?;
    let mut store = Store { paths };
    fs::create_dir_all(store.paths.system())?;
    let lock = store.lock()?;
    if store.paths.config().try_exists()? {
        return Err(Error::Conflict(
            "storage has already been initialized".into(),
        ));
    }
    let config = Config {
        layout: DATA_LAYOUT,
        listen,
        recycle_dir: store.paths.recycle.clone(),
        password_hash,
        data_disks: store.register_disks(DiskKind::Data, &[])?,
        recycle_disks: store.register_disks(DiskKind::Recycle, &[])?,
    };
    store.bind(&config);
    store.validate_disks(false)?;
    store.save_config(&config)?;
    State::open(store, config, lock, false)?;
    println!("Storage initialized");
    Ok(())
}

fn configured(
    data: &Path,
    recycle: Option<&Path>,
    verify: bool,
) -> Result<(Store, Config, fs::File)> {
    let mut store = Store {
        paths: Layout::new(data.canonicalize()?, PathBuf::new()),
    };
    let lock = store.lock()?;
    let config = store.config()?;
    let root = if verify {
        config.recycle_dir.clone()
    } else {
        config.recycle_dir.canonicalize()?
    };
    if let Some(path) = recycle
        && path.canonicalize().or_else(|error| {
            if verify {
                std::path::absolute(path)
            } else {
                Err(error)
            }
        })? != root
    {
        return Err(invalid(
            "recycle directory differs from the initialized binding",
        ));
    }
    store.paths.recycle = root;
    disjoint(&store.paths)?;
    store.bind(&config);
    store.validate_disks(verify)?;
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

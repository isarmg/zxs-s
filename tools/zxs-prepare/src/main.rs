mod build;
mod bundle;
mod disk;
mod paths;
mod web;

use std::{io, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;

fn invalid(message: impl Into<String>) -> Error {
    io::Error::other(message.into()).into()
}

#[derive(Parser)]
#[command(version, about = "独立准备 ZXS 数据盘与回收盘")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 读取磁盘状态和当前目录的项目，以 JSON 输出。
    Scan {
        disk: PathBuf,
        #[arg(long, default_value = "")]
        within: String,
    },
    /// 为已有且为空的目录写入盘身份。
    Init {
        disk: PathBuf,
        #[arg(long, value_enum)]
        kind: disk::DiskKind,
    },
    /// 原地准备已有数据盘。可重复指定要单独打成 ZIP 的相对路径。
    Prepare {
        disk: PathBuf,
        /// 服务端数据根下的挂载目录名，默认使用所选目录名。
        #[arg(long)]
        mount_name: Option<String>,
        #[arg(long, value_name = "RELATIVE_PATH")]
        bundle: Vec<String>,
    },
    /// 完成已写入切换记录的磁盘准备。
    Resume { disk: PathBuf },
    /// 打开本机浏览器界面。
    Ui,
}

fn run(command: Command) -> Result<()> {
    let text = match command {
        Command::Scan { disk, within } => {
            serde_json::to_string_pretty(&disk::scan(&disk, &within)?)?
        }
        Command::Init { disk, kind } => serde_json::to_string_pretty(&disk::init(&disk, kind)?)?,
        Command::Prepare {
            disk,
            mount_name,
            bundle,
        } => serde_json::to_string_pretty(&disk::prepare(&disk, mount_name.as_deref(), &bundle)?)?,
        Command::Resume { disk } => serde_json::to_string_pretty(&disk::resume(&disk)?)?,
        Command::Ui => return web::run(),
    };
    println!("{text}");
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zxs-prepare: {error}");
            ExitCode::FAILURE
        }
    }
}

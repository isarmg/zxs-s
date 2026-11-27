use std::{env, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return Ok(());
    }

    println!("cargo::rerun-if-changed=windows/zxs-client.rc");
    println!("cargo::rerun-if-changed=windows/zxs-client.ico");
    println!("cargo::rerun-if-env-changed=RC");

    let resource = PathBuf::from(env::var_os("OUT_DIR").ok_or("Missing Cargo OUT_DIR")?)
        .join("zxs-client.res");
    let compiler = env::var_os("RC").map_or_else(resource_compiler, PathBuf::from);
    let status = Command::new(&compiler)
        .current_dir("windows")
        .args(["/nologo", "/fo"])
        .arg(&resource)
        .arg("zxs-client.rc")
        .status()
        .map_err(|error| {
            format!(
                "Could not run {} to compile the Windows icon: {error}",
                compiler.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "{} could not compile the Windows icon: {status}",
            compiler.display()
        )
        .into());
    }
    println!("cargo::rustc-link-arg-bin=zxs-cli={}", resource.display());
    Ok(())
}

fn resource_compiler() -> PathBuf {
    #[cfg(windows)]
    if let Some(compiler) = find_msvc_tools::find_windows_sdk(env::consts::ARCH).and_then(|sdk| {
        sdk.path()
            .map(|bin| bin.join("rc.exe"))
            .find(|path| path.is_file())
    }) {
        return compiler;
    }

    if cfg!(windows) { "rc.exe" } else { "llvm-rc" }.into()
}

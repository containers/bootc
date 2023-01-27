use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use fn_error_context::context;
use xshell::{cmd, Shell};

const NAME: &str = "bootc";
const VENDORPATH: &str = "target/vendor.tar.zstd";

fn main() {
    if let Err(e) = try_main() {
        eprintln!("{e:?}");
        std::process::exit(1);
    }
}

const TASKS: &[(&'static str, fn(&Shell) -> Result<()>)] = &[
    ("vendor", vendor),
    ("package", package),
    ("package-srpm", package_srpm),
];

fn try_main() -> Result<()> {
    let task = std::env::args().nth(1);
    let sh = xshell::Shell::new()?;
    if let Some(cmd) = task.as_deref() {
        let f = TASKS
            .into_iter()
            .find_map(|(k, f)| (*k == cmd).then_some(*f))
            .unwrap_or(print_help);
        f(&sh)?;
    } else {
        print_help(&sh)?;
    }
    Ok(())
}

fn vendor(sh: &Shell) -> Result<()> {
    let target = VENDORPATH;
    cmd!(
        sh,
        "cargo vendor-filterer --prefix=vendor --format=tar.zstd {target}"
    )
    .run()?;
    Ok(())
}

fn gitrev_to_version(v: &str) -> String {
    let v = v.trim().trim_start_matches('v');
    v.replace('-', ".")
}

#[context("Finding gitrev")]
fn gitrev(sh: &Shell) -> Result<String> {
    if let Ok(rev) = cmd!(sh, "git describe --tags").ignore_stderr().read() {
        Ok(gitrev_to_version(&rev))
    } else {
        let mut desc = cmd!(sh, "git describe --tags --always").read()?;
        desc.insert_str(0, "0.");
        Ok(desc)
    }
}

struct Package {
    version: String,
    srcpath: Utf8PathBuf,
}

#[context("Packaging")]
fn impl_package(sh: &Shell) -> Result<Package> {
    let v = gitrev(sh)?;
    let namev = format!("{NAME}-{v}");
    let p = Utf8Path::new("target").join(format!("{namev}.tar.zstd"));
    let o = File::create(&p)?;
    let prefix = format!("{namev}/");
    let st = Command::new("git")
        .args([
            "archive",
            "--format=tar",
            "--prefix",
            prefix.as_str(),
            "HEAD",
        ])
        .stdout(Stdio::from(o))
        .status()?;
    if !st.success() {
        anyhow::bail!("Failed to run {st:?}");
    }
    Ok(Package {
        version: v,
        srcpath: p,
    })
}

fn package(sh: &Shell) -> Result<()> {
    let p = impl_package(sh)?.srcpath;
    println!("Generated: {p}");
    Ok(())
}

fn impl_srpm(sh: &Shell) -> Result<Utf8PathBuf> {
    let pkg = impl_package(sh)?;
    vendor(sh)?;
    let td = tempfile::tempdir_in("target").context("Allocating tmpdir")?;
    let td = td.into_path();
    let td: &Utf8Path = td.as_path().try_into().unwrap();
    let srcpath = td.join(pkg.srcpath.file_name().unwrap());
    std::fs::rename(pkg.srcpath, srcpath)?;
    let v = pkg.version;
    let vendorpath = td.join(format!("{NAME}-{v}-vendor.tar.zstd"));
    std::fs::rename(VENDORPATH, vendorpath)?;
    {
        let specin = File::open(format!("contrib/packaging/{NAME}.spec"))
            .map(BufReader::new)
            .context("Opening spec")?;
        let mut o = File::create(td.join(format!("{NAME}.spec"))).map(BufWriter::new)?;
        for line in specin.lines() {
            let line = line?;
            if line.starts_with("Version:") {
                writeln!(o, "# Replaced by cargo xtask package-srpm")?;
                writeln!(o, "Version: {v}")?;
            } else {
                writeln!(o, "{}", line)?;
            }
        }
    }
    let d = sh.push_dir(td);
    let mut cmd = cmd!(sh, "rpmbuild");
    for k in [
        "_sourcedir",
        "_specdir",
        "_builddir",
        "_srcrpmdir",
        "_rpmdir",
    ] {
        cmd = cmd.arg("--define");
        cmd = cmd.arg(format!("{k} {td}"));
    }
    cmd.arg("--define")
        .arg(format!("_buildrootdir {td}/.build"))
        .args(["-bs", "bootc.spec"])
        .run()?;
    drop(d);
    let mut srpm = None;
    for e in std::fs::read_dir(td)? {
        let e = e?;
        let n = e.file_name();
        let n = if let Some(n) = n.to_str() {
            n
        } else {
            continue;
        };
        if n.ends_with(".src.rpm") {
            srpm = Some(td.join(n));
            break;
        }
    }
    let srpm = srpm.ok_or_else(|| anyhow::anyhow!("Failed to find generated .src.rpm"))?;
    let dest = Utf8Path::new("target").join(srpm.file_name().unwrap());
    std::fs::rename(&srpm, &dest)?;
    Ok(dest)
}

fn package_srpm(sh: &Shell) -> Result<()> {
    let srpm = impl_srpm(sh)?;
    println!("Generated: {srpm}");
    Ok(())
}

fn print_help(_sh: &Shell) -> Result<()> {
    println!("Tasks:");
    for (name, _) in TASKS {
        println!("  - {name}");
    }
    Ok(())
}

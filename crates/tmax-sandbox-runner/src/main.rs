use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use anyhow::anyhow;

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, anyhow, bail};
    use nix::mount::{MsFlags, mount};
    use nix::sched::{CloneFlags, unshare};
    use nix::unistd::{Gid, Uid, execvp};
    use std::ffi::{CString, OsStr};
    use std::fs;
    use std::path::PathBuf;

    #[derive(Debug)]
    pub struct Args {
        readable: Vec<PathBuf>,
        writable: Vec<PathBuf>,
        exec: String,
        args: Vec<String>,
    }

    impl Args {
        pub fn parse(raw: Vec<String>) -> Result<Self> {
            let mut readable = Vec::new();
            let mut writable = Vec::new();
            let mut exec = None;
            let mut args = Vec::new();

            let mut i = 0usize;
            while i < raw.len() {
                match raw[i].as_str() {
                    "--readable" => {
                        i += 1;
                        let Some(value) = raw.get(i) else {
                            bail!("--readable requires a path");
                        };
                        readable.push(PathBuf::from(value));
                    }
                    "--writable" => {
                        i += 1;
                        let Some(value) = raw.get(i) else {
                            bail!("--writable requires a path");
                        };
                        writable.push(PathBuf::from(value));
                    }
                    "--exec" => {
                        i += 1;
                        let Some(value) = raw.get(i) else {
                            bail!("--exec requires a value");
                        };
                        exec = Some(value.to_string());
                    }
                    "--" => {
                        args.extend(raw[i + 1..].iter().cloned());
                        break;
                    }
                    other => bail!("unknown argument: {other}"),
                }
                i += 1;
            }

            let exec = exec.ok_or_else(|| anyhow!("missing --exec"))?;
            Ok(Self {
                readable,
                writable,
                exec,
                args,
            })
        }
    }

    pub fn run(args: Args) -> Result<()> {
        if args.writable.is_empty() && args.readable.is_empty() {
            bail!("at least one --writable or --readable path is required for sandbox runner");
        }

        unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
            .context("failed to unshare user/mount namespaces")?;
        map_current_user_to_root().context("failed to map uid/gid")?;

        mount(
            None::<&str>,
            "/",
            None::<&str>,
            MsFlags::MS_REC | MsFlags::MS_PRIVATE,
            None::<&str>,
        )
        .context("failed to set private mount propagation")?;

        mount(
            Some("/"),
            "/",
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None::<&str>,
        )
        .context("failed to bind-mount root")?;
        mount(
            Some("/"),
            "/",
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .context("failed to remount root read-only")?;

        // Readable paths: bind-mount read-only
        for path in &args.readable {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create readable path {}", path.display()))?;
            mount(
                Some(path),
                path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .with_context(|| format!("failed to bind readable path {}", path.display()))?;
            mount(
                Some(path),
                path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
                None::<&str>,
            )
            .with_context(|| {
                format!(
                    "failed to remount readable path read-only {}",
                    path.display()
                )
            })?;
        }

        // Writable paths: bind-mount read-write
        for path in &args.writable {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create writable path {}", path.display()))?;
            mount(
                Some(path),
                path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REC,
                None::<&str>,
            )
            .with_context(|| format!("failed to bind writable path {}", path.display()))?;
            mount(
                Some(path),
                path,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REMOUNT,
                None::<&str>,
            )
            .with_context(|| format!("failed to remount writable path {}", path.display()))?;
        }

        let mut argv = Vec::with_capacity(args.args.len() + 1);
        argv.push(args.exec);
        argv.extend(args.args);
        exec_command(&argv).context("exec failed")
    }

    fn map_current_user_to_root() -> Result<()> {
        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();

        let _ = fs::write("/proc/self/setgroups", "deny\n");
        fs::write("/proc/self/uid_map", format!("0 {uid} 1\n"))?;
        fs::write("/proc/self/gid_map", format!("0 {gid} 1\n"))?;
        Ok(())
    }

    fn exec_command(argv: &[String]) -> Result<()> {
        if argv.is_empty() {
            bail!("empty argv");
        }
        let cstr = |s: &OsStr| -> Result<CString> {
            CString::new(s.as_encoded_bytes())
                .map_err(|_| anyhow!("argument contains interior NUL: {:?}", s))
        };

        let prog = cstr(OsStr::new(&argv[0]))?;
        let mut args = Vec::with_capacity(argv.len());
        for arg in argv {
            args.push(cstr(OsStr::new(arg))?);
        }

        execvp(&prog, &args).context("execvp failed")?;
        Ok(())
    }
}

fn main() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let parsed = linux::Args::parse(std::env::args().skip(1).collect())?;
        linux::run(parsed)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = std::env::args();
        Err(anyhow!("tmax-sandbox-runner is only supported on Linux"))
    }
}

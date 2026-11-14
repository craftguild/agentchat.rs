//! Authoritative Git worktree/common-directory lookup without trust overrides.

use super::fileio;
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    ffi::OsString,
    fs,
    io::Read,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(super) struct Repository {
    pub common: PathBuf,
    pub exclude: PathBuf,
}
#[derive(Debug, Default)]
pub(super) struct Layout {
    pub boundary: Option<PathBuf>,
    pub repository: Option<Repository>,
}
struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn marker(path: &Path, names: &[&str]) -> Result<bool> {
    for ancestor in path.ancestors() {
        for name in names {
            match fs::symlink_metadata(ancestor.join(name)) {
                Ok(_) => return Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(fileio::io(error)),
            }
        }
    }
    Ok(false)
}
fn capture(stream: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.take(16_385).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn run(path: &Path, args: &[&str], deadline: Instant) -> Result<Option<Output>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Inherited hook routing must not select a different repository than -C.
    // Keep all trust/configuration controls, including safe.directory, intact.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
    ] {
        command.env_remove(key);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(fileio::io(error)),
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| fileio::conflict("Git stdout is unavailable."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| fileio::conflict("Git stderr is unavailable."))?;
    let (closed, receiver) = mpsc::sync_channel(2);
    let stdout_closed = closed.clone();
    let out = thread::spawn(move || {
        let result = capture(stdout);
        let _ = stdout_closed.try_send(());
        result
    });
    let err = thread::spawn(move || {
        let result = capture(stderr);
        let _ = closed.try_send(());
        result
    });
    let mut open_streams = 2;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(fileio::io)? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // Pipe completion wakes the owner promptly instead of imposing a sleep
        // quantum on every Git call. EOF is not process exit: a child can close
        // both streams and keep running, so retain bounded polling/deadline and
        // ownership of Child for killing/reaping that exact subprocess.
        let delay = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(5));
        if open_streams == 0 {
            thread::sleep(delay);
        } else {
            match receiver.recv_timeout(delay) {
                Ok(()) => open_streams -= 1,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => open_streams = 0,
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| fileio::conflict("Git output reader failed."))?
        .map_err(fileio::io)?;
    let stderr = err
        .join()
        .map_err(|_| fileio::conflict("Git error reader failed."))?
        .map_err(fileio::io)?;
    let status = status.ok_or_else(|| {
        Error::new(
            ErrorCode::Busy,
            "Git discovery exceeded its five-second deadline.",
        )
    })?;
    if stdout.len() > 16_384 || stderr.len() > 16_384 {
        return Err(fileio::conflict(
            "Git discovery output exceeds its bounded size.",
        ));
    }
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}
fn git_error(output: &Output) -> Error {
    let mut error = fileio::conflict(
        "Git refused project discovery; its trust and repository controls were preserved.",
    );
    error.details = serde_json::json!({"git_error":String::from_utf8_lossy(&output.stderr)});
    error
}
fn path(path: &Path, args: &[&str], deadline: Instant) -> Result<PathBuf> {
    let output = run(path, args, deadline)?
        .ok_or_else(|| fileio::conflict("Git is required to resolve this repository."))?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    let bytes = output
        .stdout
        .strip_suffix(b"\n")
        .ok_or_else(|| fileio::conflict("Git returned an invalid path record."))?;
    fs::canonicalize(PathBuf::from(OsString::from_vec(bytes.to_vec()))).map_err(fileio::io)
}
pub(super) fn inspect(cwd: &Path) -> Result<Layout> {
    // No cached absence: each operation rechecks the ancestry. A normal
    // worktree has .git (directory or gitfile); a bare repository requires HEAD.
    // These are conservative hints only. When present, Git still decides the
    // boundary and enforces its ordinary trust/configuration controls.
    if !marker(cwd, &[".git", "HEAD"])? {
        return Ok(Layout::default());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let Some(output) = run(cwd, &["--is-inside-work-tree"], deadline)? else {
        return Err(fileio::conflict(
            "Git is required to resolve possible repository metadata.",
        ));
    };
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("not a git repository")
            && !marker(cwd, &[".git"])?
        {
            return Ok(Layout::default());
        }
        return Err(git_error(&output));
    }
    match output.stdout.as_slice() {
        b"true\n" => {
            let root = path(
                cwd,
                &["--path-format=absolute", "--show-toplevel"],
                deadline,
            )?;
            let common = path(
                cwd,
                &["--path-format=absolute", "--git-common-dir"],
                deadline,
            )?;
            // info itself exists in ordinary repositories; exclude may not.
            let exclude = common.join("info").join("exclude");
            Ok(Layout {
                boundary: Some(root.clone()),
                repository: Some(Repository { common, exclude }),
            })
        }
        b"false\n" => Ok(Layout {
            boundary: Some(path(cwd, &["--absolute-git-dir"], deadline)?),
            repository: None,
        }),
        _ => Err(fileio::conflict(
            "Git returned an invalid worktree classification.",
        )),
    }
}

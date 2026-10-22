//! Bounded subprocess execution.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::{MediaError, MediaResult};

/// Keep at most this much of stderr for error messages.
const STDERR_LIMIT: usize = 16 * 1024;

pub(crate) struct Output {
    pub stdout: Vec<u8>,
}

pub(crate) fn command(program: &Path, args: &[OsString]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // If the future is dropped (job cancelled, lease lost, app quitting) the child dies too.
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // Don't flash a console window from the GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs to completion within `timeout`, collecting stdout. `on_line` sees each stdout line as it
/// arrives (used for FFmpeg `-progress` output).
pub(crate) async fn run(
    tool: &'static str,
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    mut on_line: impl FnMut(&str) + Send,
) -> MediaResult<Output> {
    let mut child = command(program, args).spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            MediaError::ToolMissing {
                tool,
                env_var: if tool == "ffprobe" {
                    crate::tools::FFPROBE_ENV
                } else {
                    crate::tools::FFMPEG_ENV
                },
            }
        } else {
            MediaError::Io(e)
        }
    })?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");

    let work = async {
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > STDERR_LIMIT {
                            let cut = buf.len() - STDERR_LIMIT;
                            buf.drain(..cut);
                        }
                    }
                }
            }
            buf
        });
        let mut collected = Vec::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            on_line(&line);
            collected.extend_from_slice(line.as_bytes());
            collected.push(b'\n');
        }
        let status = child.wait().await?;
        let stderr = stderr_task.await.unwrap_or_default();
        Ok::<_, MediaError>((status, collected, stderr))
    };

    let (status, stdout, stderr) = match tokio::time::timeout(timeout, work).await {
        Ok(result) => result?,
        Err(_) => {
            // `child` was moved into `work`, which has been dropped, killing the process.
            return Err(MediaError::Timeout {
                tool,
                after: timeout,
            });
        }
    };
    if !status.success() {
        return Err(MediaError::Failed {
            tool,
            status: status.to_string(),
            stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
        });
    }
    Ok(Output { stdout })
}

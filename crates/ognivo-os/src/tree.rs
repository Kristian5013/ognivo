//! A child and all it starts, ended as a whole.

use std::io;
use std::process::ExitStatus;

use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

/// How [`Tree::stop`] asks the tree to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// SIGINT, as a user's Ctrl+C.
    Interrupt,
    /// SIGTERM.
    Terminate,
    /// SIGKILL: no asking.
    Kill,
}

/// A child and all it starts: on Unix the leader of a process group of its own; on
/// Windows the first process of a job, which ends with ognivo however ognivo ends, and
/// which opens no console window. Dropped, it kills what is left of the tree, so what a
/// command left running in the background ends with it.
#[derive(Debug)]
pub struct Tree(imp::Tree);

impl Tree {
    /// Starts `cmd` as the root of a new tree.
    pub fn spawn(cmd: Command) -> io::Result<Self> {
        imp::Tree::spawn(cmd).map(Self)
    }

    /// The root's process id, until it is waited for.
    pub fn id(&self) -> Option<u32> {
        self.0.id()
    }

    pub fn stdin(&mut self) -> Option<ChildStdin> {
        self.0.stdin()
    }

    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.0.stdout()
    }

    pub fn stderr(&mut self) -> Option<ChildStderr> {
        self.0.stderr()
    }

    /// Waits for the root: what it started may still run.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.0.wait().await
    }

    /// Asks every process of the tree to end, `how`. Windows has no asking for a process
    /// without a console of its own: there each `Stop` kills.
    pub fn stop(&mut self, how: Stop) {
        self.0.stop(how);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.0.stop(Stop::Kill);
    }
}

#[cfg(unix)]
mod imp {
    use rustix::process::{Pid, Signal, kill_process_group};

    use super::*;

    #[derive(Debug)]
    pub struct Tree {
        child: tokio::process::Child,
        group: Option<Pid>,
    }

    impl Tree {
        pub fn spawn(mut cmd: Command) -> io::Result<Self> {
            cmd.process_group(0).kill_on_drop(true);
            let child = cmd.spawn()?;
            let group = child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw);
            Ok(Self { child, group })
        }

        pub fn id(&self) -> Option<u32> {
            self.child.id()
        }

        pub fn stdin(&mut self) -> Option<ChildStdin> {
            self.child.stdin.take()
        }

        pub fn stdout(&mut self) -> Option<ChildStdout> {
            self.child.stdout.take()
        }

        pub fn stderr(&mut self) -> Option<ChildStderr> {
            self.child.stderr.take()
        }

        pub async fn wait(&mut self) -> io::Result<ExitStatus> {
            self.child.wait().await
        }

        pub fn stop(&mut self, how: Stop) {
            let signal = match how {
                Stop::Interrupt => Signal::INT,
                Stop::Terminate => Signal::TERM,
                Stop::Kill => Signal::KILL,
            };
            if let Some(group) = self.group {
                let _ = kill_process_group(group, signal);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use process_wrap::tokio::{ChildWrapper, CommandWrap};

    use super::*;

    #[derive(Debug)]
    pub struct Tree {
        child: Box<dyn ChildWrapper>,
    }

    impl Tree {
        pub fn spawn(cmd: Command) -> io::Result<Self> {
            let mut wrap = CommandWrap::from(cmd);
            crate::tree_wrap(&mut wrap);
            Ok(Self {
                child: wrap.spawn()?,
            })
        }

        pub fn id(&self) -> Option<u32> {
            self.child.id()
        }

        pub fn stdin(&mut self) -> Option<ChildStdin> {
            self.child.stdin().take()
        }

        pub fn stdout(&mut self) -> Option<ChildStdout> {
            self.child.stdout().take()
        }

        pub fn stderr(&mut self) -> Option<ChildStderr> {
            self.child.stderr().take()
        }

        /// The job's own wait reaps the whole job; its inner child is the root alone.
        pub async fn wait(&mut self) -> io::Result<ExitStatus> {
            self.child.inner_mut().wait().await
        }

        pub fn stop(&mut self, _how: Stop) {
            let _ = self.child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt as _;

    use super::*;

    fn bash(script: &str) -> Command {
        let mut cmd = Command::new(crate::shell::bash().unwrap());
        cmd.arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped());
        cmd
    }

    #[tokio::test]
    async fn the_root_is_waited_for_and_a_dropped_tree_ends_its_rest() {
        // the root prints its child's pid and leaves it running
        let mut tree = Tree::spawn(bash("sleep 30 & echo $!")).unwrap();
        let mut out = String::new();
        let mut stdout = tree.stdout().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), tree.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        // the background `sleep` holds stdout open; its pid is the first line
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), stdout.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        out.push_str(std::str::from_utf8(&buf[..n]).unwrap());
        assert!(out.trim().parse::<u32>().is_ok(), "{out:?}");
        drop(tree);
        // with the tree gone, nothing holds the pipe: it ends
        let rest = tokio::time::timeout(Duration::from_secs(10), async {
            let mut rest = Vec::new();
            stdout.read_to_end(&mut rest).await.map(|_| rest)
        })
        .await;
        assert!(rest.is_ok(), "the background sleep outlived its tree");
    }

    #[tokio::test]
    async fn a_stopped_tree_ends_at_once() {
        let mut tree = Tree::spawn(bash("sleep 30")).unwrap();
        tree.stop(Stop::Kill);
        let status = tokio::time::timeout(Duration::from_secs(10), tree.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
    }
}

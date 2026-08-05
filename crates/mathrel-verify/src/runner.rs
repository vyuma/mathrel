//! 外部プロセスの起動。
//!
//! Lean も Codex も外部プロセスである。ここを trait で切っておくと、
//! **Lean が入っていない環境でも出力の解釈をテストできる**。実際、この層を
//! 書いた時点の開発機に Lean は入っていない。道具の有無でテストの通り方が
//! 変わる作りにはしない。

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// プロセスの実行結果。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommandOutput {
    /// 終了コード。シグナルで死んだ場合は `None`。
    pub status: Option<i32>,
    /// 標準出力。
    pub stdout: String,
    /// 標準エラー出力。
    pub stderr: String,
}

impl CommandOutput {
    /// 正常終了したか。
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.status == Some(0)
    }

    /// 標準出力と標準エラーを合わせたもの。
    ///
    /// Lean は診断を stdout に出したり stderr に出したりする。呼ぶ側で
    /// 気にしなくて済むようにまとめる。
    #[must_use]
    pub fn combined(&self) -> String {
        let mut combined = String::with_capacity(self.stdout.len() + self.stderr.len() + 1);
        combined.push_str(&self.stdout);
        if !self.stdout.is_empty() && !self.stderr.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&self.stderr);
        combined
    }
}

/// プロセスを起動できなかった理由。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RunError {
    /// 実行ファイルが見つからない。
    NotFound(String),
    /// 制限時間内に終わらなかった。
    Timeout(Duration),
    /// その他。
    Io(String),
}

impl core::fmt::Display for RunError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RunError::NotFound(program) => write!(f, "{program} が見つかりません"),
            RunError::Timeout(limit) => write!(f, "{} 秒で打ち切りました", limit.as_secs()),
            RunError::Io(message) => write!(f, "起動に失敗しました: {message}"),
        }
    }
}

/// 外部プロセスを起動するもの。
pub trait CommandRunner {
    /// `program` を `args` で起動し、`stdin` を流し込んで結果を待つ。
    fn run(&self, program: &str, args: &[&str], stdin: &str) -> Result<CommandOutput, RunError>;
}

/// 本物のプロセスを起動する実装。
#[derive(Clone, Debug)]
pub struct ProcessRunner {
    /// 制限時間。
    pub timeout: Duration,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
        }
    }
}

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[&str], stdin: &str) -> Result<CommandOutput, RunError> {
        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RunError::NotFound(program.to_owned()))
            }
            Err(error) => return Err(RunError::Io(error.to_string())),
        };

        if let Some(mut pipe) = child.stdin.take() {
            // 相手が先に終了して SIGPIPE になるのは異常ではない。無視する。
            let _ = pipe.write_all(stdin.as_bytes());
        }

        // `std::process` には待ち時間つきの wait がない。別スレッドで待って
        // チャネルで取る。時間切れなら子プロセスを殺す。
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = child.wait_with_output();
            // 受け取り手が既に諦めていても構わない。
            let _ = sender.send(result);
        });

        match receiver.recv_timeout(self.timeout) {
            Ok(Ok(output)) => {
                let _ = handle.join();
                Ok(CommandOutput {
                    status: output.status.code(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
            Ok(Err(error)) => Err(RunError::Io(error.to_string())),
            Err(_) => Err(RunError::Timeout(self.timeout)),
        }
    }
}

/// テスト用。決められた応答を順に返す。
///
/// 引数に `.lean` で終わるものがあれば、**呼び出しの時点で**その中身を読んで
/// 記録する。本物のプロセスが見るはずのものを、テスト側でも見られるように
/// するため。呼び出しが終わると一時ファイルは消えるので、後から読むことは
/// できない。
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    responses: std::cell::RefCell<std::collections::VecDeque<Result<CommandOutput, RunError>>>,
    calls: std::cell::RefCell<Vec<(String, Vec<String>, String)>>,
    files: std::cell::RefCell<Vec<String>>,
}

impl ScriptedRunner {
    /// 応答の並びを与えて作る。
    #[must_use]
    pub fn new(responses: Vec<Result<CommandOutput, RunError>>) -> Self {
        Self {
            responses: std::cell::RefCell::new(responses.into()),
            calls: std::cell::RefCell::new(Vec::new()),
            files: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// 正常終了 1 回だけを返すもの。
    #[must_use]
    pub fn succeeding(stdout: &str) -> Self {
        Self::new(vec![Ok(CommandOutput {
            status: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })])
    }

    /// 失敗 1 回だけを返すもの。
    #[must_use]
    pub fn failing(stderr: &str) -> Self {
        Self::new(vec![Ok(CommandOutput {
            status: Some(1),
            stdout: String::new(),
            stderr: stderr.to_owned(),
        })])
    }

    /// これまでに渡された (プログラム, 引数, 標準入力)。
    #[must_use]
    pub fn calls(&self) -> Vec<(String, Vec<String>, String)> {
        self.calls.borrow().clone()
    }

    /// 呼び出しのたびに読み取った `.lean` ファイルの中身。
    #[must_use]
    pub fn files_seen(&self) -> Vec<String> {
        self.files.borrow().clone()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, program: &str, args: &[&str], stdin: &str) -> Result<CommandOutput, RunError> {
        for arg in args {
            if arg.ends_with(".lean") {
                if let Ok(contents) = std::fs::read_to_string(arg) {
                    self.files.borrow_mut().push(contents);
                }
            }
        }
        self.calls.borrow_mut().push((
            program.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdin.to_owned(),
        ));
        self.responses
            .borrow_mut()
            .pop_front()
            .unwrap_or(Err(RunError::Io("応答が尽きました".to_owned())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_joins_both_streams() {
        let output = CommandOutput {
            status: Some(0),
            stdout: "out".to_owned(),
            stderr: "err".to_owned(),
        };
        assert_eq!(output.combined(), "out\nerr");
        assert!(output.succeeded());
    }

    #[test]
    fn combined_does_not_add_a_stray_newline() {
        let output = CommandOutput {
            status: Some(0),
            stdout: "out".to_owned(),
            stderr: String::new(),
        };
        assert_eq!(output.combined(), "out");
    }

    #[test]
    fn the_scripted_runner_records_what_it_was_asked() {
        let runner = ScriptedRunner::succeeding("ok");
        let output = runner.run("lean", &["--stdin"], "theorem x : True := trivial");
        assert_eq!(output.expect("応答").stdout, "ok");

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "lean");
        assert_eq!(calls[0].1, vec!["--stdin".to_owned()]);
        assert!(calls[0].2.contains("theorem"));
    }

    #[test]
    fn a_missing_program_is_reported_as_not_found() {
        let runner = ProcessRunner {
            timeout: Duration::from_secs(5),
        };
        let error = runner
            .run("mathrel-no-such-program-exists", &[], "")
            .expect_err("あるはずがない");
        assert_eq!(
            error,
            RunError::NotFound("mathrel-no-such-program-exists".to_owned())
        );
    }

    #[test]
    fn a_real_process_round_trips_stdin() {
        let runner = ProcessRunner {
            timeout: Duration::from_secs(30),
        };
        let output = runner.run("cat", &[], "hello").expect("cat は在る");
        assert!(output.succeeded());
        assert_eq!(output.stdout, "hello");
    }

    #[test]
    fn a_slow_process_is_cut_off() {
        let runner = ProcessRunner {
            timeout: Duration::from_millis(200),
        };
        let error = runner.run("sleep", &["30"], "").expect_err("時間切れ");
        assert!(matches!(error, RunError::Timeout(_)), "{error:?}");
    }
}

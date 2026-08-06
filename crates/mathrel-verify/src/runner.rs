//! 外部プロセスの起動。
//!
//! Lean も Codex も外部プロセスである。ここを trait で切っておくと、
//! **Lean が入っていない環境でも出力の解釈をテストできる**。実際、この層を
//! 書いた時点の開発機に Lean は入っていない。道具の有無でテストの通り方が
//! 変わる作りにはしない。

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    /// 契約: **`run` が返るとき、起動したプロセス群はもう残っていない。**
    ///
    /// `lake env lean` は孫として `lean` を起動する。子だけを見ていると、
    /// (1) タイムアウトで子を殺しても孫が生き残る、(2) 子が正常終了しても
    /// 孫がパイプを握っている限り EOF が来ず、**タイムアウトと無関係に**
    /// 読み取りが張り付く（`sh -c 'sleep 15 & exit 0'` で実測 15 秒）。
    /// そこで子を自分のプロセスグループの先頭にして、終了時にグループごと
    /// 掃除する。
    fn run(&self, program: &str, args: &[&str], stdin: &str) -> Result<CommandOutput, RunError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 子を新しいプロセスグループの先頭にする（pgid = 子の pid）。
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RunError::NotFound(program.to_owned()))
            }
            Err(error) => return Err(RunError::Io(error.to_string())),
        };
        let group = child.id();

        // stdin は専用スレッドで流し込む。本体スレッドで書くと、入力と出力が
        // 共に 64KB を超えたとき、まだ読み手のいない stdout が詰まって親子が
        // 互いを待つ（cat に 1MB 渡すと固まる形になっていた）。
        let stdin_pipe = child.stdin.take();
        let stdin_data = stdin.to_owned();
        let stdin_writer = thread::spawn(move || {
            if let Some(mut pipe) = stdin_pipe {
                // 相手が先に終了して SIGPIPE になるのは異常ではない。無視する。
                let _ = pipe.write_all(stdin_data.as_bytes());
            }
        });

        // 出力は別スレッドで吸う。パイプが詰まると子プロセスが書き込みで
        // 止まり、永遠に終わらなくなるため（wait より先に読み手が要る）。
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_reader = thread::spawn(move || drain(stdout_pipe));
        let stderr_reader = thread::spawn(move || drain(stderr_pipe));

        let waited = wait_with_deadline(&mut child, self.timeout);

        // グループの掃除。タイムアウト時（子は殺したが孫が残り得る）と、
        // 読み手がまだ EOF を見ていないとき（= 誰かがパイプを握っている）に
        // 行う。両読み手が既に終わっているなら書き端は閉じており、殺す相手も
        // パイプを詰まらせる相手もいない。
        if waited.is_err() || !stdout_reader.is_finished() || !stderr_reader.is_finished() {
            kill_group(group);
        }

        // グループが死んでいるので、これらの join は必ず返る（EOF / EPIPE）。
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        let _ = stdin_writer.join();

        let status = waited?;
        Ok(CommandOutput {
            status: status.code(),
            stdout,
            stderr,
        })
    }
}

/// プロセスグループ全体へ SIGKILL を送る。誰も残っていなければ何もしない。
///
/// 対象は `process_group(0)` で自分専用に作ったグループなので、他所の
/// プロセスを巻き込まない。子の回収後に pgid が再利用される理論上の窓は
/// あるが、これは timeout(1) など既存のグループ殺し全般と同じ前提である。
///
/// libc の `kill(2)` を直接呼ばず `kill` コマンドを起動するのは、この
/// クレートが `#![forbid(unsafe_code)]` だからである。掃除の経路だけの
/// 追加コストであり、失敗しても best-effort（`let _`）でよい。
fn kill_group(group: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(not(unix))]
    let _ = group;
}

/// 締切つきで子プロセスの終了を待つ。**時間切れなら殺して、回収まで行う。**
///
/// `Child` を `wait_with_output` に渡してしまうと所有権ごと消費され、
/// タイムアウト側の分岐から殺せなくなる（以前の実装はそうなっていて、
/// 「殺す」というコメントだけが残っていた。issue #52）。`try_wait` の
/// ポーリングなら `Child` が手元に残るので、締切超過で `kill → wait` できる。
fn wait_with_deadline(child: &mut Child, timeout: Duration) -> Result<ExitStatus, RunError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // 殺すだけでは足りない。wait で回収しないとゾンビが残る。
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Timeout(timeout));
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(RunError::Io(error.to_string())),
        }
    }
}

/// パイプを最後まで読む。UTF-8 でないバイトは置換文字に落とす。
fn drain(pipe: Option<impl Read>) -> String {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
        let started = Instant::now();
        let error = runner.run("sleep", &["30"], "").expect_err("時間切れ");
        assert!(matches!(error, RunError::Timeout(_)), "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "打ち切りは締切の直後に返る（30 秒待ってはいない）"
        );
    }

    /// 時間切れの子プロセスは**実際に殺され、回収されている**。
    ///
    /// 以前の実装は `Timeout` を返すだけで子を放置していた（issue #52）。
    /// このテストは `wait_with_deadline` の後で `try_wait` が終了済みを
    /// 返すこと（= kill と回収が済んでいること）まで確かめる。
    #[test]
    fn a_timed_out_child_is_killed_and_reaped() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let error = wait_with_deadline(&mut child, Duration::from_millis(100))
            .expect_err("時間切れになるはず");
        assert!(matches!(error, RunError::Timeout(_)), "{error:?}");

        let reaped = child.try_wait().expect("try_wait");
        assert!(
            reaped.is_some(),
            "子プロセスが回収済みであること（放置されていたら None）"
        );
        assert!(!reaped.expect("status").success(), "kill で死んでいる");
    }

    /// 孫がパイプを握ったまま生きていても、run はすぐ返る。
    ///
    /// 子が正常終了しても、孫が stdout の書き端を継いでいると EOF が来ない。
    /// グループ掃除の前は、タイムアウトと無関係に孫の寿命ぶん張り付いた
    /// （`sleep 15 & exit 0` で実測 15 秒）。パイプ内のデータは失われない。
    #[test]
    fn a_grandchild_holding_the_pipe_does_not_hang_the_runner() {
        let runner = ProcessRunner {
            timeout: Duration::from_secs(10),
        };
        let started = Instant::now();
        let output = runner
            .run("sh", &["-c", "echo before; sleep 30 & exit 0"], "")
            .expect("子は正常終了している");
        assert!(output.succeeded());
        assert!(output.stdout.contains("before"), "{output:?}");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "孫の sleep 30 を待ってはいない"
        );
    }

    /// タイムアウト時、孫も含めてグループごと死ぬ。
    #[test]
    fn a_timeout_kills_the_whole_process_group() {
        let runner = ProcessRunner {
            timeout: Duration::from_millis(300),
        };
        let started = Instant::now();
        let error = runner
            .run("sh", &["-c", "sleep 30 & sleep 30; true"], "")
            .expect_err("時間切れ");
        assert!(matches!(error, RunError::Timeout(_)), "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "グループを殺せば読み取りの join もすぐ返る"
        );
    }

    /// 入力と出力が共に大きくてもデッドロックしない。
    ///
    /// stdin を本体スレッドで書くと、読み手が立つ前に stdout が詰まって
    /// 親子が互いを待つ（cat に 1MB 渡すと固まった）。stdin は専用スレッド。
    #[test]
    fn large_input_and_output_do_not_deadlock() {
        let runner = ProcessRunner {
            timeout: Duration::from_secs(30),
        };
        let input = "x".repeat(1 << 20);
        let output = runner.run("cat", &[], &input).expect("cat は通る");
        assert!(output.succeeded());
        assert_eq!(output.stdout.len(), input.len(), "1MB がそのまま往復する");
    }
}

//! `mathrel` — Relational Math Kernel の対話シェル。
//!
//! 企画書 §8 の P1 完了条件（「`x = 3` に変更すると `y` だけが再計算され、
//! 正しい値になる。CLI で確認できる」）を確認するためのもの。
//!
//! ```text
//! $ mathrel --demo
//! ```
//!
//! 標準入力がパイプでも動く。
//!
//! ```text
//! $ printf 'x = 2\ny = x + 1\n:list\n' | mathrel
//! ```

mod session;

use session::Session;
use std::io::{self, BufRead, IsTerminal, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut session = Session::new();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|arg| arg == "--demo") {
        for line in session.demo() {
            println!("{line}");
        }
        return;
    }

    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    if interactive {
        println!("mathrel — :help で命令の一覧、:quit で終了");
    }

    let mut input = String::new();
    loop {
        if interactive {
            print!("> ");
            let _ = io::stdout().flush();
        }
        input.clear();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("入力を読めませんでした: {error}");
                break;
            }
        }

        let response = session.handle(&input);
        for line in &response.lines {
            println!("{line}");
        }
        if response.quit {
            break;
        }
    }
}

const USAGE: &str = "mathrel — Relational Math Kernel の対話シェル

使い方:
  mathrel              対話モード（標準入力がパイプでも動く）
  mathrel --demo       企画書 P1 の完了条件を流す
  mathrel --help       これ

対話モードの命令は :help で出る。";

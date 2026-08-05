//! # mathrel-verify
//!
//! 「この主張は正しいか」を、証明支援系と AI を使って扱う層。
//!
//! ## 中心的な主張
//!
//! **検証はカーネルにとって「評価」と同型である。カーネルは変更しない。**
//!
//! `y = f(x)` が古くなるのと、`定理: f は非負` が古くなるのは同じ現象である。
//! どちらも `f` に依存しており、`f` が変われば両方とも作り直しになる。よって
//! 検証層は新しい追跡機構を必要とせず、既存の `next_batch` → 検証 →
//! `commit_evaluation` ループに別種のバックエンドを挿すだけでよい。
//!
//! 詳細と、どこまで出来るかの見積もりは `docs/検証層の設計.md`。
//!
//! ## 早期カットオフは、証明でこそ本質的である
//!
//! | | 再計算のコスト |
//! |---|---|
//! | 算術評価 | マイクロ秒。全部やり直しても気づかない |
//! | Lean の再検査 | 秒〜分。**全部やり直しは非現実的** |
//!
//! カーネルが `MaybeDirty` と `Dirty` を分けている設計は、算術では最適化に
//! 過ぎないが、証明では成立条件そのものになる。
//!
//! ## AI の位置
//!
//! ```text
//!    AI (Codex, Claude)  --証明の候補-->  Lean 4  --通ったものだけ-->  Proved
//!          ^                                 |
//!          +------- 失敗の理由を添えて -------+
//! ```
//!
//! [`ProofSynthesizer`] は文字列しか返せない。[`Verdict`] を返す手段を型として
//! 持たない。**AI は探索ヒューリスティックであり、権威ではない。**
//!
//! ## 信頼度は依存に沿って伝播する
//!
//! Lean が完全に検査した定理でも、AI が仮置きした補題に依存していれば、実効
//! 信頼度は [`Trust::Suggested`] にしかならない。
//!
//! ```
//! use mathrel_verify::{Obligation, ProofSpace, Trust, TrivialVerifier};
//!
//! let mut space = ProofSpace::new();
//! space.add_definition("f", "def f (t : Nat) : Nat := t");
//!
//! // 補題は人間が「これは既知」と宣言しただけ。誰も検査していない。
//! let lemma = space.add_assumption(Obligation::new("f_id", "f t = t").using(&["f"]));
//! // 定理そのものは検証器が完全に検査した。
//! let theorem = space.add_obligation(Obligation::new("main", "f t = f t").citing(&["f_id"]));
//! space.verify_all(&TrivialVerifier);
//!
//! assert_eq!(space.own_trust(lemma), Trust::Assumed);
//! assert_eq!(space.own_trust(theorem), Trust::Checked);
//!
//! // それでも、仮置きの補題に依存している以上、実効信頼度は上がらない。
//! assert_eq!(space.effective_trust(theorem), Trust::Assumed);
//! assert_eq!(space.weakest_link(), Trust::Assumed);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod fingerprint;
pub mod lean;
pub mod obligation;
pub mod runner;
pub mod space;
pub mod synth;
pub mod trust;
pub mod verdict;
pub mod verifier;

pub use fingerprint::{digest_of, to_hex};
pub use lean::LeanVerifier;
pub use obligation::{BackendId, Obligation};
pub use runner::{CommandOutput, CommandRunner, ProcessRunner, RunError, ScriptedRunner};
pub use space::{backend_label, Entry, EntryKind, ProofSpace, VerifyStats};
pub use synth::{repair, Attempt, CodexSynthesizer, ProofSynthesizer, RepairOutcome, ScriptedSynthesizer};
pub use trust::Trust;
pub use verdict::Verdict;
pub use verifier::{TrivialVerifier, Verifier};

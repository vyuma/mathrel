//! mathrel の画面。
//!
//! 企画書 §8 の P2（UI 最小）。**このフェーズはゲートである**（H3 を満た
//! さなければ P3 に進まない）ので、入力体験を最優先にしている。
//!
//! ## 構成
//!
//! - [`view`] — ワークスペースの状態を、画面が必要とする形に写す。**DOM を
//!   触らないので、ブラウザなしでテストできる**
//! - [`mathfield`] — MathLive の `<math-field>` に触るための最小の道具
//! - このファイル — 描画と操作
//!
//! UI のバグの大半は「何を見せるか」の判断ミスであって、要素の組み立てでは
//! ない。判断は [`view`] に集めてテストしてある。
//!
//! ## 提示の原則（#42〜#48、`docs/検証層の設計.md` §7.1.1）
//!
//! - **変化を知らせ、状態は求めに応じて見せる。** 常時表示の警告は慣化して
//!   壁紙になる（警報疲れ）。信頼度のニュースは動いた直後だけ出し、総量は
//!   ヘッダ、内訳はヘッダから開く
//! - **ニュースは次の操作まで残す。** 1.4 秒のフラッシュは変化盲に食われる
//! - **削除は取り消せる。** 確認ダイアログは慣化でクリックスルーされるので、
//!   undo で守る
//!
//! ## なぜ Rust で書くか
//!
//! `mathrel-host` を直接依存できるので、**Rust と JS のあいだに JSON の境界が
//! できない**。`Workspace` / `Trust` / `WeakLink` が本物の型のまま届く。
//! フィールド名を変えれば、画面のほうがコンパイルエラーになる。

mod mathfield;
mod view;

use leptos::prelude::*;
use mathrel_host::{parse_obligation_line, CellId, CellKind, Workspace};
use mathrel_verify::{Obligation, TrivialVerifier};
use std::rc::Rc;
use view::{CellView, LinkView, SpaceView, TrustChange, TrustShift, WeakLinkView};

/// 1 行ぶんのシグナル。`Workspace` が `Send + Sync` ではないので局所に置く。
type Row = (CellId, RwSignal<CellView, LocalStorage>);

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

/// 新しいセルをどう足すか。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Adding {
    /// 数式・定義。
    Expression,
    /// 検証義務。
    Theorem,
    /// 「既知」として宣言するだけのもの。
    Assumption,
}

impl Adding {
    /// 入力欄に出す例。モードの帰結を打つ前に見せる（モードエラー対策）。
    fn placeholder(self) -> &'static str {
        match self {
            Adding::Expression => "x = 2  /  f(t) = t^2 + 1",
            Adding::Theorem => "main : a = a  cites lemma",
            Adding::Assumption => "lemma : 難しい命題",
        }
    }

    /// 追加すると何が起きるかの予告。
    ///
    /// モードは注視点（入力欄）の外にあるボタンなので、**帰結の文章を入力欄の
    /// そばに常置**してモードエラーを抑える。仮置きの説明はそのまま信頼度の
    /// 教育を兼ねる。
    fn helper(self) -> &'static str {
        match self {
            Adding::Expression => "数式・定義として追加します",
            Adding::Theorem => {
                "検証義務として追加し、検証器が検査します（名前 : 命題。uses / cites は任意）"
            }
            Adding::Assumption => {
                "検査せず「既知」として置きます。依存する下流の実効信頼度は Assumed 止まりになります"
            }
        }
    }

    /// 入力欄をモードで塗り分けるための CSS クラス。
    fn css(self) -> &'static str {
        match self {
            Adding::Expression => "mode-expr",
            Adding::Theorem => "mode-thm",
            Adding::Assumption => "mode-assume",
        }
    }
}

/// 削除の取り消しに要るぶんだけ覚えておく。
///
/// 義務セルの `source` は `theorem 名前 : 命題` 形式で **uses / cites が
/// 落ちている**ので、文字列からの復元はできない。`Obligation` ごと取っておく。
#[derive(Clone, PartialEq, Debug)]
struct Removed {
    /// 表示用の原文字列。
    source: String,
    /// 復元に使う中身。
    payload: RemovedPayload,
}

/// 削除されたセルの中身。
#[derive(Clone, PartialEq, Debug)]
enum RemovedPayload {
    /// 数式セル。原文字列から再登録できる。
    Expression(String),
    /// 義務セル。
    Obligation {
        /// 義務そのもの（uses / cites を含む）。
        obligation: Obligation,
        /// 仮置きだったか。
        assumed: bool,
    },
}

#[component]
fn App() -> impl IntoView {
    // `Workspace` は `Rc<dyn Verifier>` を持つので `Send + Sync` ではない。
    // Leptos の局所ストアに置く。CSR では単一スレッドなので問題ない。
    let workspace = StoredValue::new_local({
        let mut workspace = Workspace::new();
        // v0.1 では自明な命題しか通らない。Lean を差すのは issue #23。
        workspace.set_verifier(Rc::new(TrivialVerifier));
        workspace
    });

    // 行は「セル番号 → その行のシグナル」で持つ。
    //
    // `<For>` はキーが同じ行を作り直さない。中身が変わったら**行のシグナルを
    // 差し替える**必要がある。これを怠ると、編集しても画面が変わらない
    // （実際に一度そうなった）。行ごとにシグナルを持てば、DOM ノードは
    // そのままで中身だけが更新され、入力中のフォーカスも飛ばない。
    let rows: RwSignal<Vec<Row>, LocalStorage> = RwSignal::new_local(Vec::new());
    let space = RwSignal::new(SpaceView::default());
    let draft = RwSignal::new(String::new());
    let mode = RwSignal::new(Adding::Expression);
    let note = RwSignal::new(String::new());
    // 誤モードで拒否されたとき、ワンクリックで数式として再送するための旗。
    let offer_expression = RwSignal::new(false);
    // 直近に自分で触ったセル。波及（ニュース）から除くために覚える。
    let touched = RwSignal::new(None::<CellId>);
    // 直近に削除したセル。「元に戻す」の弾。
    let removed: RwSignal<Option<Removed>, LocalStorage> = RwSignal::new_local(None);
    let show_breakdown = RwSignal::new(false);

    // フォーカスの落下防止。削除・復元・再送のボタンは押すと自分が DOM から
    // 消えるので、放っておくとフォーカスが文書先頭に落ち、キーボード利用者は
    // 現在地を失う。行き先を明示的に与える。
    let draft_ref: NodeRef<leptos::html::Input> = NodeRef::new();
    let undo_ref: NodeRef<leptos::html::Button> = NodeRef::new();
    let focus_draft = move || {
        if let Some(input) = draft_ref.get_untracked() {
            let _ = input.focus();
        }
    };
    // 「元に戻す」はニュース欄に**後から**現れるので、マウントを待って移す。
    Effect::new(move |_| {
        if removed.get().is_some() {
            if let Some(button) = undo_ref.get() {
                let _ = button.focus();
            }
        }
    });

    // 評価して画面を更新する。**前回との差分**がニュースになる。
    // 「1 つ変えたら何が古くなったか」を目で確かめられる唯一の場所である。
    let refresh = move |quiet: bool| {
        workspace.update_value(|workspace| {
            let previous = space.get_untracked();
            let stats = workspace.evaluate();
            // 起動時の一括計算はニュースではないので、静かに写す。
            let order: &[CellId] = if quiet { &[] } else { &stats.order };
            let mut view = SpaceView::of(workspace, order);
            if !quiet {
                view.trust_changes = view::trust_changes(&previous, &view);
            }

            // 既にある行は中身だけ差し替える。消えた行は落とし、増えた行は
            // 足す。並び順が変わらなければ `<For>` は何も作り直さない。
            let mut existing: Vec<Row> = rows.get_untracked();
            let mut next: Vec<Row> = Vec::with_capacity(view.cells.len());
            let mut changed_shape = existing.len() != view.cells.len();

            for (index, cell) in view.cells.iter().enumerate() {
                match existing.iter().find(|(id, _)| *id == cell.id) {
                    Some((id, signal)) => {
                        if signal.get_untracked() != *cell {
                            signal.set(cell.clone());
                        }
                        if existing.get(index).map(|(had, _)| *had) != Some(*id) {
                            changed_shape = true;
                        }
                        next.push((*id, *signal));
                    }
                    None => {
                        changed_shape = true;
                        next.push((cell.id, RwSignal::new_local(cell.clone())));
                    }
                }
            }
            existing.retain(|(id, _)| view.cells.iter().any(|cell| cell.id == *id));
            if existing.len() != next.len() {
                changed_shape = true;
            }
            if changed_shape {
                rows.set(next);
            }

            space.set(view);
        });
    };

    let submit = move || {
        let text = draft.get_untracked().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let mut rejected = None;
        let mut added = None;
        workspace.update_value(|workspace| match mode.get_untracked() {
            Adding::Expression => {
                added = Some(workspace.add_cell(&text));
            }
            other => match parse_obligation_line(&text) {
                Some(obligation) => {
                    added = Some(if other == Adding::Assumption {
                        workspace.add_assumption(obligation)
                    } else {
                        workspace.add_obligation(obligation)
                    });
                }
                None => {
                    rejected = Some(
                        "「名前 : 命題」の形で書いてください（uses / cites は任意）".to_owned(),
                    );
                }
            },
        });
        match rejected {
            Some(message) => {
                note.set(message);
                offer_expression.set(true);
            }
            None => {
                note.set(String::new());
                offer_expression.set(false);
                draft.set(String::new());
                touched.set(added);
                removed.set(None);
                refresh(false);
            }
        }
    };

    let edit = move |id: CellId, text: String| {
        workspace.update_value(|workspace| {
            let _ = workspace.update_cell(id, &text);
        });
        touched.set(Some(id));
        removed.set(None);
        refresh(false);
    };

    let remove = move |id: CellId| {
        let mut captured = None;
        workspace.update_value(|workspace| {
            if let Some(cell) = workspace.cell(id) {
                let payload = match &cell.kind {
                    CellKind::Expression => RemovedPayload::Expression(cell.source.clone()),
                    CellKind::Obligation {
                        obligation,
                        assumed,
                        ..
                    } => RemovedPayload::Obligation {
                        obligation: (**obligation).clone(),
                        assumed: *assumed,
                    },
                };
                captured = Some(Removed {
                    source: cell.source.clone(),
                    payload,
                });
            }
            let _ = workspace.remove_cell(id);
        });
        touched.set(None);
        removed.set(captured);
        refresh(false);
    };

    let restore = move || {
        let Some(gone) = removed.get_untracked() else {
            return;
        };
        let mut added = None;
        workspace.update_value(|workspace| {
            added = Some(match gone.payload {
                RemovedPayload::Expression(source) => workspace.add_cell(&source),
                RemovedPayload::Obligation {
                    obligation,
                    assumed: true,
                } => workspace.add_assumption(obligation),
                RemovedPayload::Obligation { obligation, .. } => {
                    workspace.add_obligation(obligation)
                }
            });
        });
        removed.set(None);
        touched.set(added);
        refresh(false);
        focus_draft();
    };

    // 最初の内容。空の画面より、動いているものを見せるほうが早い。
    workspace.update_value(|workspace| {
        for source in ["x = 2", "f(t) = t^2 + 1", "y = f(x)"] {
            workspace.add_cell(source);
        }
    });
    refresh(true);

    view! {
        <header class="top">
            <h1>"mathrel"</h1>
            <span class="revision">{move || format!("r{}", space.get().revision)}</span>
            <TrustSummary space=space show=show_breakdown />
        </header>

        <Show when=move || show_breakdown.get()>
            <TrustPanel space=space />
        </Show>

        <main>
            <For
                each=move || rows.get()
                key=|(id, _)| *id
                let:row
            >
                <Cell cell=row.1 edit=edit remove=remove />
            </For>
        </main>

        <footer class="compose">
            <News space=space touched=touched removed=removed undo_ref=undo_ref restore=restore />
            <form
                class="compose-row"
                on:submit=move |event: leptos::ev::SubmitEvent| {
                    event.prevent_default();
                    submit();
                }
            >
                <div class="modes" role="group" aria-label="追加の種類">
                    <ModeButton mode=mode value=Adding::Expression label="数式" />
                    <ModeButton mode=mode value=Adding::Theorem label="定理" />
                    <ModeButton mode=mode value=Adding::Assumption label="既知として置く" />
                </div>
                <input
                    class=move || format!("input draft {}", mode.get().css())
                    placeholder=move || mode.get().placeholder()
                    node_ref=draft_ref
                    prop:value=move || draft.get()
                    on:input=move |event| {
                        if let Some(text) = mathfield::value_from(&event) {
                            draft.set(text);
                        }
                    }
                />
                <button class="add" type="submit">"追加"</button>
            </form>
            <p class="helper">{move || mode.get().helper()}</p>
            <p class="note" role="alert">
                {move || note.get()}
                <Show when=move || offer_expression.get()>
                    <button
                        type="button"
                        class="offer"
                        on:click=move |_| {
                            mode.set(Adding::Expression);
                            submit();
                            focus_draft();
                        }
                    >
                        "数式として追加する"
                    </button>
                </Show>
            </p>
        </footer>
    }
}

/// 全体の信頼度を 1 行で。押すと内訳が開く。
///
/// 常設はこの 1 行だけにする。満点でないことは**総量**として常に見えるが、
/// 警告のバナーは変化の瞬間にしか出ない（警報疲れ対策、#42）。
#[component]
fn TrustSummary(space: RwSignal<SpaceView>, show: RwSignal<bool>) -> impl IntoView {
    view! {
        <button
            type="button"
            class="trustline"
            class:weak=move || space.get().has_warning()
            aria-expanded=move || show.get().to_string()
            title="押すと内訳が開きます"
            on:click=move |_| show.update(|open| *open = !*open)
        >
            {move || {
                let space = space.get();
                if space.weak_links.is_empty() {
                    format!("信頼度 {}", space.weakest_link)
                } else {
                    format!(
                        "信頼度 {} · 満点でない {} 件",
                        space.weakest_link,
                        space.weak_links.len(),
                    )
                }
            }}
        </button>
    }
}

/// 満点でないものの内訳。ヘッダから開いたときだけ出る（`:trust` 相当）。
#[component]
fn TrustPanel(space: RwSignal<SpaceView>) -> impl IntoView {
    view! {
        <section class="warning">
            <p class="headline">
                {move || {
                    let space = space.get();
                    if space.weak_links.is_empty() {
                        "満点でないものはありません".to_owned()
                    } else {
                        format!(
                            "信頼度が満点でないものが {} 件あります（全体は {}）",
                            space.weak_links.len(),
                            space.weakest_link,
                        )
                    }
                }}
            </p>
            <ul>
                <For
                    each=move || space.get().weak_links
                    key=|weak| (weak.id, weak.culprit)
                    let:weak
                >
                    <WeakLine weak=weak />
                </For>
            </ul>
            <p class="legend">
                "満点にならないのは、仮置き・sorry・AI の提案・検証器なし・検証が通らない・未解決の参照・引用の循環、のいずれかです。上流が 1 つでも該当すると、下流もそこまで落ちます。"
            </p>
        </section>
    }
}

/// 弱い環 1 件。
#[component]
fn WeakLine(weak: WeakLinkView) -> impl IntoView {
    let same = weak.id == weak.culprit;
    let culprit = format!("原因は [{}]", weak.culprit);
    let culprit_href = format!("#cell-{}", weak.culprit);
    view! {
        <li>
            <a class="cell-ref" href=format!("#cell-{}", weak.id)>{format!("[{}]", weak.id)}</a>
            <span class="trust">{weak.effective.label()}</span>
            <Show when=move || !same>
                <a class="culprit" href=culprit_href.clone()>{culprit.clone()}</a>
            </Show>
            <span class="reason">{weak.reason.clone()}</span>
        </li>
    }
}

/// 直近の操作で**変わったこと**の通知。次の操作まで残る。
///
/// 1.4 秒のフラッシュは変化盲に食われる（注視点は自分が打った入力欄にある）。
/// 消えない一覧をここに置くことで、見逃しても証拠が残る（#43）。信頼度の
/// 変化は下降 → 新規 → 回復の順で、それぞれ調子を変える（#42）。
///
/// `<section>` 自体は**常設マウント**にする。ライブリージョンは「既に DOM に
/// ある領域の中身の変化」しか読み上げられないので、`Show` で領域ごと
/// 出し入れすると、スクリーンリーダーには最初の通知が届かない。
#[component]
fn News(
    space: RwSignal<SpaceView>,
    touched: RwSignal<Option<CellId>>,
    removed: RwSignal<Option<Removed>, LocalStorage>,
    /// 「元に戻す」ボタン。削除直後にフォーカスを移す先（削除ボタンは
    /// 消えるので、放っておくとフォーカスが文書先頭に落ちる）。
    undo_ref: NodeRef<leptos::html::Button>,
    // `Show` の子は Send を要求する。LocalStorage のハンドルも StoredValue の
    // ハンドルも Send なので、境界を書けば実体は満たす。
    restore: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let changes = move || space.get().trust_changes;
    // `view!` の属性位置には `>` を含む式を書けないので、ここで畳んでおく。
    let top_changes = move || {
        let mut list = changes();
        list.truncate(4);
        list
    };
    let overflow = move || changes().len() > 4;
    let ripple = move || space.get().ripple_of(touched.get());
    let has_news = move || !changes().is_empty() || !ripple().is_empty() || removed.get().is_some();

    view! {
        <section class="news" class:has-news=has_news role="status" aria-live="polite">
            <For
                each=top_changes
                key=|change| (change.id, change.from, change.to, change.label.clone())
                let:change
            >
                <TrustNews change=change space=space />
            </For>
            <Show when=overflow>
                <p class="news-item plain">
                    // 「内訳はヘッダから」と誘導してはいけない。並び順の都合で
                    // 隠れるのはほぼ回復（Recovery）であり、回復は満点なので
                    // ヘッダの内訳（weak_links）には載らない。案内が空振りする。
                    {move || format!("…他 {} 件", changes().len() - 4)}
                </p>
            </Show>
            <Show when=move || !ripple().is_empty()>
                <p class="news-item plain">
                    "この操作で再計算: "
                    <RefList links=Signal::derive(ripple) />
                </p>
            </Show>
            <Show when=move || removed.get().is_some()>
                <p class="news-item plain">
                    {move || {
                        removed
                            .get()
                            .map(|gone| format!("「{}」を削除しました", gone.source))
                            .unwrap_or_default()
                    }}
                    <button
                        type="button"
                        class="undo"
                        node_ref=undo_ref
                        on:click=move |_| restore()
                    >
                        "元に戻す"
                    </button>
                </p>
            </Show>
        </section>
    }
}

/// 信頼度の変化 1 件。
#[component]
fn TrustNews(change: TrustChange, space: RwSignal<SpaceView>) -> impl IntoView {
    let tone = match change.shift() {
        TrustShift::Drop => "warn",
        TrustShift::NewWeak => "info",
        TrustShift::Recovery => "good",
    };
    let text = match change.shift() {
        TrustShift::Drop => {
            let cause = match change.culprit {
                Some(culprit) if culprit != change.id => {
                    format!(
                        "。原因は {}（{}）",
                        space.get_untracked().label_of(culprit),
                        change.reason
                    )
                }
                _ => format!("（{}）", change.reason),
            };
            format!(
                "⚠ {} の実効信頼度が下がりました: {} → {}{}",
                change.label,
                change.from.map(|trust| trust.label()).unwrap_or("?"),
                change.to,
                cause,
            )
        }
        TrustShift::NewWeak => format!(
            "{} を追加しました。実効信頼度は {}（{}）",
            change.label, change.to, change.reason,
        ),
        TrustShift::Recovery => format!(
            "{} の実効信頼度が {} → {} に上がりました",
            change.label,
            change.from.map(|trust| trust.label()).unwrap_or("?"),
            change.to,
        ),
    };
    let target = format!("#cell-{}", change.culprit.unwrap_or(change.id));
    view! {
        <p class=format!("news-item {tone}")>
            {text}
            " "
            <a class="link" href=target>"該当セルへ"</a>
        </p>
    }
}

/// セル 1 つ。
#[component]
fn Cell(
    cell: RwSignal<CellView, LocalStorage>,
    edit: impl Fn(CellId, String) + Copy + 'static,
    remove: impl Fn(CellId) + Copy + 'static,
) -> impl IntoView {
    let id = cell.with_untracked(|cell| cell.id);
    let tone = move || cell.with(|cell| cell.status.tone.class());

    // 静かなセルは畳む。異常・義務は**常時展開**で、畳むことを許さない —
    // 展開されていること自体が異常の信号になる（#48）。
    let quiet = move || cell.with(CellView::is_quiet);
    let opened = RwSignal::new(false);
    let show_meta = move || opened.get() || !quiet();

    view! {
        <article
            id=format!("cell-{id}")
            class=move || format!("cell {}", tone())
            class:recomputed=move || cell.with(|cell| cell.recomputed)
        >
            <div class="line">
                <span class="id">{format!("[{id}]")}</span>
                <Field
                    value=Signal::derive(move || cell.with(|cell| cell.source.clone()))
                    plain=Signal::derive(move || cell.with(|cell| cell.is_obligation))
                    on_commit=move |text| edit(id, text)
                />
                <Show when=move || cell.with(|cell| cell.value.is_some())>
                    <span class="value">
                        {move || cell.with(|cell| {
                            format!("= {}", cell.value.clone().unwrap_or_default())
                        })}
                    </span>
                </Show>
                <Show when=move || cell.with(|cell| cell.recomputed)>
                    <span class="chip">"再計算"</span>
                </Show>
                <span class="tools">
                    <Show when=quiet>
                        <button
                            type="button"
                            class="disclose"
                            aria-expanded=move || opened.get().to_string()
                            on:click=move |_| opened.update(|open| *open = !*open)
                        >
                            {move || if opened.get() { "閉じる" } else { "詳細" }}
                        </button>
                    </Show>
                    <button
                        type="button"
                        class="remove"
                        on:click=move |_| remove(id)
                        title="このセルを削除（元に戻せます）"
                        aria-label="このセルを削除"
                    >
                        "×"
                    </button>
                </span>
            </div>

            <Show when=show_meta>
                <div class="meta">
                    <span class=move || format!("badge {}", tone())>
                        {move || cell.with(|cell| cell.status.label.clone())}
                    </span>
                    <Show when=move || cell.with(|cell| !cell.status.detail.is_empty())>
                        <span class="detail">
                            {move || cell.with(|cell| cell.status.detail.clone())}
                        </span>
                    </Show>
                    <Show when=move || cell.with(|cell| cell.is_obligation)>
                        <span class="trust">{move || cell.with(CellView::trust_label)}</span>
                    </Show>
                    <Links
                        label="依存先"
                        links=Signal::derive(move || cell.with(|cell| cell.dependencies.clone()))
                    />
                    <Links
                        label="依存元"
                        links=Signal::derive(move || cell.with(|cell| cell.dependents.clone()))
                    />
                </div>
            </Show>
        </article>
    }
}

/// 依存関係のリスト表示。
///
/// 企画書 §8 の P2 は「グラフ描画でなくリスト表示でよい」としている。
/// 名前で読ませ、アンカーで飛ばす。`href="#cell-n"` は JS なしでスクロール
/// し、CSS の `:target` が着地先を光らせる。キーボードでも同じ経路が使える。
#[component]
fn Links(label: &'static str, links: Signal<Vec<LinkView>>) -> impl IntoView {
    view! {
        <Show when=move || !links.get().is_empty()>
            <span class="links">
                {label}
                " "
                <RefList links=links />
            </span>
        </Show>
    }
}

/// セルへの参照の並び。
#[component]
fn RefList(links: Signal<Vec<LinkView>>) -> impl IntoView {
    view! {
        // key に label も含める。id だけだと、リンク先が改名されたときに
        // `<For>` が行を再利用して**古い名前を表示し続ける**（rows と同じ罠。
        // App 冒頭のコメント参照）。
        <For each=move || links.get() key=|link| (link.id, link.label.clone()) let:link>
            <a
                class="link"
                href=format!("#cell-{}", link.id)
                title=format!("[{}] へ移動", link.id)
            >
                {link.label.clone()}
            </a>
        </For>
    }
}

/// 入力欄。MathLive があれば `<math-field>`、無ければ素の入力。
///
/// 検証義務は `名前 : 命題 uses a cites b` という**文字列**なので、数式として
/// 扱うと壊れる。そちらは常に素の入力にする。
#[component]
fn Field(
    value: Signal<String>,
    plain: Signal<bool>,
    on_commit: impl Fn(String) + Copy + 'static,
) -> impl IntoView {
    // MathLive の有無と、義務かどうかは作成時に決まる。途中で変わらない。
    let use_math = !plain.get_untracked() && mathfield::available();
    let commit = move |event: web_sys::Event| {
        if let Some(text) = mathfield::value_from(&event) {
            on_commit(text);
        }
    };

    if use_math {
        view! {
            <math-field class="input math" on:change=commit>
                {move || value.get()}
            </math-field>
        }
        .into_any()
    } else {
        // `prop:value` は値が変わったときだけ書き込まれる。入力中に
        // シグナルが動かない限り、打ちかけの文字を消さない。
        view! {
            <input class="input plain" prop:value=move || value.get() on:change=commit />
        }
        .into_any()
    }
}

/// 追加の種類を選ぶボタン。
#[component]
fn ModeButton(mode: RwSignal<Adding>, value: Adding, label: &'static str) -> impl IntoView {
    view! {
        <button
            type="button"
            class="mode"
            class:active=move || mode.get() == value
            aria-pressed=move || (mode.get() == value).to_string()
            on:click=move |_| mode.set(value)
        >
            {label}
        </button>
    }
}

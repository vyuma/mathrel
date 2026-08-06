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
//! ## なぜ Rust で書くか
//!
//! `mathrel-host` を直接依存できるので、**Rust と JS のあいだに JSON の境界が
//! できない**。`Workspace` / `Trust` / `WeakLink` が本物の型のまま届く。
//! フィールド名を変えれば、画面のほうがコンパイルエラーになる。

mod mathfield;
mod view;

use leptos::prelude::*;
use mathrel_host::{parse_obligation_line, CellId, Workspace};
use mathrel_verify::TrivialVerifier;
use std::rc::Rc;
use view::{CellView, SpaceView, WeakLinkView};

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

    // 評価して画面を更新する。**再計算されたセルを覚えておく**のが要点で、
    // 「1 つ変えたら何が古くなったか」を目で確かめられる唯一の場所である。
    let refresh = move || {
        workspace.update_value(|workspace| {
            let stats = workspace.evaluate();
            let view = SpaceView::of(workspace, &stats.order);

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

    let submit = move |_| {
        let text = draft.get_untracked().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let mut rejected = None;
        workspace.update_value(|workspace| match mode.get_untracked() {
            Adding::Expression => {
                workspace.add_cell(&text);
            }
            other => match parse_obligation_line(&text) {
                Some(obligation) => {
                    if other == Adding::Assumption {
                        workspace.add_assumption(obligation);
                    } else {
                        workspace.add_obligation(obligation);
                    }
                }
                None => {
                    rejected = Some(
                        "「名前 : 命題」の形で書いてください（uses / cites は任意）".to_owned(),
                    );
                }
            },
        });
        match rejected {
            Some(message) => note.set(message),
            None => {
                note.set(String::new());
                draft.set(String::new());
                refresh();
            }
        }
    };

    let edit = move |id: CellId, text: String| {
        workspace.update_value(|workspace| {
            let _ = workspace.update_cell(id, &text);
        });
        refresh();
    };

    let remove = move |id: CellId| {
        workspace.update_value(|workspace| {
            let _ = workspace.remove_cell(id);
        });
        refresh();
    };

    // 最初の内容。空の画面より、動いているものを見せるほうが早い。
    workspace.update_value(|workspace| {
        for source in ["x = 2", "f(t) = t^2 + 1", "y = f(x)"] {
            workspace.add_cell(source);
        }
    });
    refresh();

    view! {
        <header class="top">
            <h1>"mathrel"</h1>
            <span class="revision">{move || format!("r{}", space.get().revision)}</span>
            <Trustline space=space />
        </header>

        <Warning space=space />

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
            <div class="modes">
                <ModeButton mode=mode value=Adding::Expression label="数式" />
                <ModeButton mode=mode value=Adding::Theorem label="定理" />
                <ModeButton mode=mode value=Adding::Assumption label="既知として置く" />
            </div>
            <Draft draft=draft />
            <button class="add" on:click=submit>"追加"</button>
            <p class="note">{move || note.get()}</p>
        </footer>
    }
}

/// 全体の信頼度を 1 行で。
#[component]
fn Trustline(space: RwSignal<SpaceView>) -> impl IntoView {
    view! {
        <span class="trustline" class:weak=move || space.get().has_warning()>
            {move || format!("信頼度 {}", space.get().weakest_link)}
        </span>
    }
}

/// 満点でないものの警告。
///
/// 最下部にひっそり出すのではなく、**上部に常に出す**。見落とされたら意味が
/// ない（`docs/検証層の設計.md` §7.1）。
#[component]
fn Warning(space: RwSignal<SpaceView>) -> impl IntoView {
    view! {
        <Show when=move || space.get().has_warning()>
            <section class="warning">
                <p class="headline">
                    {move || {
                        let space = space.get();
                        format!(
                            "信頼度が満点でないものが {} 件あります（全体は {}）",
                            space.weak_links.len(),
                            space.weakest_link,
                        )
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
        </Show>
    }
}

/// 弱い環 1 件。
#[component]
fn WeakLine(weak: WeakLinkView) -> impl IntoView {
    let same = weak.id == weak.culprit;
    let culprit = format!("原因は [{}]", weak.culprit);
    view! {
        <li>
            <span class="cell-ref">{format!("[{}]", weak.id)}</span>
            <span class="trust">{weak.effective.label()}</span>
            <Show when=move || !same>
                <span class="culprit">{culprit.clone()}</span>
            </Show>
            <span class="reason">{weak.reason.clone()}</span>
        </li>
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

    view! {
        <article
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
                <button class="remove" on:click=move |_| remove(id) title="削除">"×"</button>
            </div>

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
                    ids=Signal::derive(move || cell.with(|cell| cell.dependencies.clone()))
                />
                <Links
                    label="依存元"
                    ids=Signal::derive(move || cell.with(|cell| cell.dependents.clone()))
                />
            </div>
        </article>
    }
}

/// 依存関係のリスト表示。
///
/// 企画書 §8 の P2 は「グラフ描画でなくリスト表示でよい」としている。
#[component]
fn Links(label: &'static str, ids: Signal<Vec<CellId>>) -> impl IntoView {
    view! {
        <Show when=move || !ids.get().is_empty()>
            <span class="links">
                {label}
                " "
                {move || {
                    ids.get().iter().map(|id| format!("[{id}]")).collect::<Vec<_>>().join(" ")
                }}
            </span>
        </Show>
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

/// 新しいセルの入力欄。
#[component]
fn Draft(draft: RwSignal<String>) -> impl IntoView {
    view! {
        <input
            class="input draft"
            placeholder="x = 2  /  thm : 0 <= x uses x"
            prop:value=move || draft.get()
            on:input=move |event| {
                if let Some(text) = mathfield::value_from(&event) {
                    draft.set(text);
                }
            }
        />
    }
}

/// 追加の種類を選ぶボタン。
#[component]
fn ModeButton(mode: RwSignal<Adding>, value: Adding, label: &'static str) -> impl IntoView {
    view! {
        <button
            class="mode"
            class:active=move || mode.get() == value
            on:click=move |_| mode.set(value)
        >
            {label}
        </button>
    }
}

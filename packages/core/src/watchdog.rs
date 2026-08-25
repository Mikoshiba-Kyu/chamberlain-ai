//! JS 実行の番犬 (#59)。
//!
//! JS は単一の V8 isolate を専用スレッドに閉じ込めて**直列**に実行する
//! ([`crate::worker`] のモジュール doc)。したがって 1 つのトリガーの `tick()` が
//! 返ってこないと、他のトリガーは永久に `tick()` されない。心拍そのものは Rust 側で
//! 生きているので「心臓は止まらない」は守られるが、秘書は実質的に仕事をしなくなる。
//!
//! 焼き込みだけなら「自分で書いたコードのバグ」で済む。実行時登録 (#58) で他人や AI が
//! 書いたトリガーを受け入れる以上、悪意が無くても事故として起きる。
//!
//! # 止め方が 2 通り要る
//!
//! 返ってこない JS には性質の違う 2 種類がある。**片方だけでは塞がらない。**
//!
//! | 種類 | 例 | 止める仕組み |
//! |---|---|---|
//! | 非同期の待ち | 解決しない Promise、返ってこない `fetch` | rustyscript の `RuntimeOptions::timeout` |
//! | 同期のループ | `while (true) {}` | この番犬 (V8 の `terminate_execution`) |
//!
//! `RuntimeOptions::timeout` の実体は tokio の `timeout` で、**JS が event loop に制御を
//! 返すこと**を前提にしている。同期の無限ループは制御を返さないので、tokio のタイマーは
//! そもそも動く機会が無い (実測: 60 秒待っても返らない)。V8 の実行そのものを別スレッドから
//! 止めるしかない。
//!
//! 逆に、非同期の待ちを番犬では止められない。待っている間 JS は動いていないので、
//! `terminate_execution` は「次に JS が動くとき」まで効かない。2 つは相補的である。
//!
//! 2 つが**同じ予算で必ず一組**入るように、Runtime の生成は [`Watchdog::guarding`] が
//! 持つ。片方だけ設定した Runtime は作れず、予算が食い違うこともない (食い違うと
//! [`Watchdog::guard`] が返す文言が嘘になる)。
//!
//! # 中断フラグは必ず降ろす
//!
//! `terminate_execution` は isolate に中断フラグを立てる。**このフラグはトリガーごとでは
//! なく isolate ごと**なので、降ろさずに放置すると次に動かした別のトリガーが自分のせいで
//! ない中断を食う。締め切りちょうどに JS が自力で返ってきた場合は「叩いたが結果は成功」に
//! なるので、成否に関わらず降ろす。[`Watchdog::guard`] がその対を持つ唯一の場所である。
//!
//! # やらないこと
//!
//! **トリガーごとの isolate 分離。** 悪意ある 1 つが `chamberlain` グローバルを差し替えて
//! 他のトリガーの呼び出しを盗る経路 (#55) はこれでは塞がらない。rustyscript の構造上重く
//! メモリも増えるので、単一開発者のフレームワークでは割に合わないと判断した。認識だけ
//! しておく。

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 番犬スレッドと JS スレッドが共有する状態。
#[derive(Default)]
struct Watch {
    /// 実行中の締め切り。`None` は「今 JS は動いていない」。
    deadline: Option<Instant>,
    /// 直前の実行を番犬が止めたか。[`Watchdog::arm`] でリセットする。
    fired: bool,
    /// 番犬スレッドを畳む。
    shutdown: bool,
}

/// JS 実行 1 回に予算を与え、超えたら V8 を外から止める。
///
/// 1 つの Runtime に 1 つ。JS は直列にしか動かないので、締め切りも常に 1 つで足りる。
pub(crate) struct Watchdog {
    shared: Arc<(Mutex<Watch>, Condvar)>,
    /// [`Drop`] で畳むために持つ。`None` になるのは drop 中だけ。
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    /// 予算付きの Runtime を立てる。**JS を動かす Runtime はここからしか作らない。**
    ///
    /// 止め方 2 通り (モジュール doc) を同じ予算で一組にするのがこの関数の役目で、
    /// `options.timeout` に何が入っていても `budget` で上書きする。
    pub(crate) fn guarding(
        budget: Duration,
        options: rustyscript::RuntimeOptions,
    ) -> Result<(rustyscript::Runtime, Self), rustyscript::Error> {
        let mut runtime = rustyscript::Runtime::new(rustyscript::RuntimeOptions {
            timeout: budget,
            ..options
        })?;
        // handle は Send なので、番犬スレッドから同期の無限ループを外して止められる。
        let handle = runtime.deno_runtime().v8_isolate().thread_safe_handle();
        Ok((
            runtime,
            Self::spawn(move || {
                handle.terminate_execution();
            }),
        ))
    }

    /// 番犬スレッドを立てる。`interrupt` は締め切り超過時に叩く相手。
    ///
    /// 本番の相手は V8 の `IsolateHandle` ([`Self::guarding`])。閉包を取るのは、
    /// 間に合ったか / 叩いたかの判定を V8 抜きで試験するため — ここで間違えると
    /// 「叩き漏らす」か「無関係な実行を巻き込む」のどちらかになり、どちらも本物の
    /// Runtime 越しには再現しづらい。
    fn spawn(interrupt: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new((Mutex::new(Watch::default()), Condvar::new()));
        let for_thread = Arc::clone(&shared);
        let thread = std::thread::spawn(move || watch(&for_thread, interrupt));
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// 番犬の下で JS を 1 回動かす。
    ///
    /// 戻り値のエラーは表示用の文字列。予算超過は 2 通りの止め方のどちらで止まっても
    /// **同じ 1 文**に正規化する — 観測面から見て意味があるのは「予算を使い切って
    /// 中断された」の 1 つで、どちらの仕組みが拾ったかは内部事情である。
    /// 予算はこの 1 回のためのもの (#81)。トリガーが `maxRuntimeSec` を宣言して
    /// いればその値が入る。
    ///
    /// **止め方 2 通りのうち、ここで指定した値ちょうどで止まるのは同期のループだけ。**
    /// 非同期の待ちを止めるのは `RuntimeOptions::timeout` で、こちらは Runtime 生成時に
    /// 固定である ([`Self::guarding`] に渡した値 = そのレーンの上限)。
    ///
    /// つまり `budget` がレーンの上限より短いとき、**非同期側の締め切りはレーンの上限に
    /// なる**。`maxRuntimeSec: 120` を宣言したトリガーが解決しない Promise を待つと、
    /// 同期ループなら 120 秒で止まるが、待ちは長レーンの上限 (30 分) まで伸びる。
    ///
    /// これを許しているのは、`chamberlain.*` の非同期 op が個別に上限を持っているため
    /// (`ai.complete` は 90 秒、`http.fetch` は 30 秒)。解決しない待ちを作るには
    /// `new Promise(() => {})` を自分で書く必要があり、その場合でも占有されるのは
    /// そのレーンの作業員 1 人に留まる。
    ///
    /// **既定のまま (宣言なし) のトリガーには非対称が無い。** 標準レーンの上限と
    /// `JS_BUDGET` が同じ値なので、どちらの止め方も 110 秒ちょうどで効く。
    pub(crate) fn guard<T>(
        &self,
        budget: Duration,
        runtime: &mut rustyscript::Runtime,
        f: impl FnOnce(&mut rustyscript::Runtime) -> Result<T, rustyscript::Error>,
    ) -> Result<T, String> {
        self.arm(budget);
        let result = f(runtime);
        let interrupted = self.disarm();
        if interrupted {
            // モジュール doc「中断フラグは必ず降ろす」。
            runtime
                .deno_runtime()
                .v8_isolate()
                .cancel_terminate_execution();
        }

        result.map_err(|e| {
            // 番犬が叩いたときの V8 側のエラーは "Unknown error" で、そのままでは
            // 原因が読み取れない。ここで言い換える。
            if interrupted || matches!(e, rustyscript::Error::Timeout(_)) {
                format!(
                    "実行が {} 秒の上限を超えたため中断しました",
                    budget.as_secs()
                )
            } else {
                e.to_string()
            }
        })
    }

    /// 締め切りを立てる。番犬は寝ているか前の締め切りを待っているので、起こして読み直させる。
    fn arm(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        let (lock, cvar) = &*self.shared;
        let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
        w.deadline = Some(deadline);
        w.fired = false;
        cvar.notify_one();
    }

    /// 締め切りを降ろす。戻り値は**この実行を番犬が止めたか**。
    ///
    /// **起こさない。** 番犬が叩くかどうかはロックの中で決まる ([`watch`]) ので、ここで
    /// `deadline` を落とせば以後叩かれないことは確定している。起こすと JS 実行 1 回ごとに
    /// 番犬スレッドを起床させることになり、得るものが無い。次の [`Self::arm`] が
    /// 新しい締め切りで起こす。
    fn disarm(&self) -> bool {
        let (lock, _) = &*self.shared;
        let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
        w.deadline = None;
        w.fired
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
            w.shutdown = true;
            cvar.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 番犬スレッドの本体。
///
/// 起こされる理由は arm / shutdown の 2 つと締め切り到達だが、**どれでもループ先頭で
/// 状態を読み直す**ので区別しない。spurious wakeup も同じ経路で吸収される。
fn watch(shared: &(Mutex<Watch>, Condvar), interrupt: impl Fn()) {
    let (lock, cvar) = shared;
    let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if w.shutdown {
            return;
        }
        let Some(deadline) = w.deadline else {
            w = cvar.wait(w).unwrap_or_else(|e| e.into_inner());
            continue;
        };
        let now = Instant::now();
        if now < deadline {
            w = cvar
                .wait_timeout(w, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            continue;
        }

        // 締め切り到達。**ロックを持ったまま**叩く。ここで手放すと、間に合った実行の
        // disarm が先に通って `fired` を読み逃し、中断フラグが次の実行に漏れる。
        // `terminate_execution` はフラグを立てるだけで待たないので、持ったままでよい。
        w.fired = true;
        w.deadline = None;
        interrupt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 叩かれた回数を数えるだけの番犬。V8 を起こさずに締め切りの判定だけを見る。
    fn counting(budget_ms: u64) -> (Watchdog, Duration, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let dog = Watchdog::spawn(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (dog, Duration::from_millis(budget_ms), hits)
    }

    fn hits(counter: &Arc<AtomicUsize>) -> usize {
        counter.load(Ordering::SeqCst)
    }

    /// 予算を超えた実行は叩かれ、`disarm` がそれを申告する。
    #[test]
    fn an_overrunning_run_is_interrupted() {
        let (dog, budget, counter) = counting(50);
        dog.arm(budget);
        std::thread::sleep(Duration::from_millis(150));
        assert!(dog.disarm(), "予算超過が申告されていない");
        assert_eq!(hits(&counter), 1);
    }

    /// 間に合った実行は叩かれない。
    #[test]
    fn a_run_within_the_budget_is_left_alone() {
        let (dog, budget, counter) = counting(1000);
        dog.arm(budget);
        std::thread::sleep(Duration::from_millis(20));
        assert!(!dog.disarm());
        assert_eq!(hits(&counter), 0);
    }

    /// 予算は実行ごとに引き直される。短い実行を積み重ねても叩かれない
    /// (累計は予算を超えるが、1 回あたりは超えていない)。
    #[test]
    fn each_run_gets_a_fresh_budget() {
        let (dog, budget, counter) = counting(150);
        for _ in 0..6 {
            dog.arm(budget);
            std::thread::sleep(Duration::from_millis(40));
            assert!(!dog.disarm());
        }
        assert_eq!(hits(&counter), 0);
    }

    /// 叩かれた実行の次は、また間に合う判定に戻る (`fired` が持ち越されない)。
    #[test]
    fn the_verdict_does_not_leak_into_the_next_run() {
        let (dog, budget, counter) = counting(50);
        dog.arm(budget);
        std::thread::sleep(Duration::from_millis(150));
        assert!(dog.disarm());

        dog.arm(budget);
        std::thread::sleep(Duration::from_millis(10));
        assert!(!dog.disarm(), "前の実行の判定が持ち越されている");
        assert_eq!(hits(&counter), 1);
    }

    /// JS が動いていない間は締め切りが無いので、いくら待っても叩かれない。
    #[test]
    fn an_idle_watchdog_never_fires() {
        let (_dog, _budget, counter) = counting(50);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(hits(&counter), 0);
    }

    // ---- 本物の V8 ------------------------------------------------------------

    fn load(
        runtime: &mut rustyscript::Runtime,
        name: &str,
        source: &str,
    ) -> rustyscript::ModuleHandle {
        let module = rustyscript::Module::new(name, source);
        runtime
            .load_module(&module)
            .unwrap_or_else(|e| panic!("{name} をロードできない: {e}"))
    }

    fn tick<T: serde::de::DeserializeOwned>(
        dog: &Watchdog,
        budget: Duration,
        runtime: &mut rustyscript::Runtime,
        handle: &rustyscript::ModuleHandle,
    ) -> Result<T, String> {
        dog.guard(budget, runtime, |rt| {
            rt.call_function(Some(handle), "tick", rustyscript::json_args!())
        })
    }

    /// 返ってこないトリガーは中断され、**同じ Runtime に載っている別のトリガーは
    /// その後も動く**。
    ///
    /// 中断そのものより後半が要点で、Runtime が壊れるなら中断だけでは足りず作り直しが
    /// 要ることになる。止め方 2 通りのどちらで止まっても同じ結末になることを、同じ
    /// 手順で 2 回確かめる。
    fn a_hang_is_cut_and_the_neighbour_survives(name: &str, source: &str) {
        let budget = Duration::from_millis(300);
        let (mut runtime, dog) =
            Watchdog::guarding(budget, Default::default()).expect("failed to init JS runtime");
        let hung = load(&mut runtime, name, source);
        let neighbour = load(
            &mut runtime,
            "neighbour.js",
            "export function tick() { return 'まだ動きます'; }",
        );

        let err = tick::<serde_json::Value>(&dog, budget, &mut runtime, &hung)
            .expect_err("返ってこない JS が中断されていない");
        assert!(err.contains("上限を超えた"), "{err}");

        let survived: String =
            tick(&dog, budget, &mut runtime, &neighbour).expect("隣のトリガーが動かない");
        assert_eq!(survived, "まだ動きます");
    }

    /// **#59 の完了条件。** 同期の無限ループ (番犬が止める側)。
    #[test]
    fn a_spinning_trigger_does_not_stop_the_next_one() {
        a_hang_is_cut_and_the_neighbour_survives(
            "spin.js",
            "export function tick() { while (true) {} }",
        );
    }

    /// 解決しない Promise (`RuntimeOptions::timeout` が止める側)。
    #[test]
    fn an_awaited_promise_that_never_resolves_is_also_cut() {
        a_hang_is_cut_and_the_neighbour_survives(
            "hang.js",
            "export async function tick() { await new Promise(() => {}); }",
        );
    }

    /// 予算内で失敗した JS のエラーは言い換えない。「中断された」と「JS が投げた」を
    /// 混ぜると、観測面から原因の切り分けができなくなる。
    #[test]
    fn an_ordinary_js_error_keeps_its_own_message() {
        let budget = Duration::from_secs(10);
        let (mut runtime, dog) =
            Watchdog::guarding(budget, Default::default()).expect("failed to init JS runtime");
        let boom = load(
            &mut runtime,
            "boom.js",
            "export function tick() { throw new Error('壊れています'); }",
        );

        let err = tick::<serde_json::Value>(&dog, budget, &mut runtime, &boom)
            .expect_err("投げたのに成功している");
        assert!(err.contains("壊れています"), "{err}");
        assert!(!err.contains("上限を超えた"), "{err}");
    }
}

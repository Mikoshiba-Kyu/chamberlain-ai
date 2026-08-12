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
//! # 中断フラグは必ず降ろす
//!
//! `terminate_execution` は isolate に中断フラグを立てる。**このフラグはトリガーごとでは
//! なく isolate ごと**なので、降ろさずに放置すると次に動かした別のトリガーが自分のせいで
//! ない中断を食う。番犬が叩いたら必ず `cancel_terminate_execution` で降ろす —
//! [`Watchdog::guard`] がその対を持つ唯一の場所である。
//!
//! 締め切りちょうどに JS が自力で返ってきた場合、「叩いたが結果は成功」という組み合わせが
//! 起きる。これも中断フラグは立っているので、成否に関わらず降ろす。
//!
//! # やらないこと
//!
//! **トリガーごとの isolate 分離。** 悪意ある 1 つが `chamberlain` グローバルを差し替えて
//! 他のトリガーの呼び出しを盗る経路 (#55) はこれでは塞がらない。rustyscript の構造上重く
//! メモリも増えるので、単一開発者のフレームワークでは割に合わないと判断した。認識だけ
//! しておく。

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 番犬が締め切りで叩く相手。本番は V8 の `IsolateHandle`、テストは記録用の fake。
///
/// trait を挟むのは、間に合ったか / 叩いたかの判定を V8 抜きで試験するため。ここで
/// 間違えると「叩き漏らす」か「無関係な実行を巻き込む」のどちらかになり、どちらも
/// 本物の Runtime 越しには再現しづらい。
pub(crate) trait Interrupt: Send + 'static {
    fn interrupt(&self);
}

impl Interrupt for deno_core::v8::IsolateHandle {
    fn interrupt(&self) {
        self.terminate_execution();
    }
}

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
    budget: Duration,
    /// [`Drop`] で畳むために持つ。`None` になるのは drop 中だけ。
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    /// 番犬スレッドを立てる。`interrupt` は締め切り超過時に叩く相手。
    pub(crate) fn spawn<I: Interrupt>(budget: Duration, interrupt: I) -> Self {
        let shared = Arc::new((Mutex::new(Watch::default()), Condvar::new()));
        let for_thread = Arc::clone(&shared);
        let thread = std::thread::spawn(move || watch(&for_thread, &interrupt));
        Self {
            shared,
            budget,
            thread: Some(thread),
        }
    }

    /// 番犬の下で JS を 1 回動かす。**JS を動かす経路は必ずここを通す。**
    ///
    /// 戻り値のエラーは表示用の文字列。予算超過は 2 通りの止め方 (モジュール doc) の
    /// どちらで止まっても**同じ 1 文**に正規化する — 観測面から見て意味があるのは
    /// 「予算を使い切って中断された」の 1 つで、どちらの仕組みが拾ったかは内部事情である。
    pub(crate) fn guard<T>(
        &self,
        runtime: &mut rustyscript::Runtime,
        f: impl FnOnce(&mut rustyscript::Runtime) -> Result<T, rustyscript::Error>,
    ) -> Result<T, String> {
        self.arm();
        let result = f(runtime);
        let interrupted = self.disarm();
        if interrupted {
            // 叩いた以上、成否に関わらず降ろす (モジュール doc「中断フラグは必ず降ろす」)。
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
                    self.budget.as_secs()
                )
            } else {
                e.to_string()
            }
        })
    }

    /// 締め切りを立てる。
    fn arm(&self) {
        let (lock, cvar) = &*self.shared;
        let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
        w.deadline = Some(Instant::now() + self.budget);
        w.fired = false;
        cvar.notify_all();
    }

    /// 締め切りを降ろす。戻り値は**この実行を番犬が止めたか**。
    fn disarm(&self) -> bool {
        let (lock, cvar) = &*self.shared;
        let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
        w.deadline = None;
        cvar.notify_all();
        w.fired
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut w = lock.lock().unwrap_or_else(|e| e.into_inner());
            w.shutdown = true;
            cvar.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 番犬スレッドの本体。
///
/// 起こされる理由は arm / disarm / shutdown の 3 つあるが、**どれでもループ先頭で状態を
/// 読み直す**ので区別しない。spurious wakeup も同じ経路で吸収される。
fn watch<I: Interrupt>(shared: &(Mutex<Watch>, Condvar), interrupt: &I) {
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
        interrupt.interrupt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 叩かれた回数を数えるだけの [`Interrupt`]。
    impl Interrupt for Arc<AtomicUsize> {
        fn interrupt(&self) {
            self.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn watchdog(budget_ms: u64) -> (Watchdog, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let dog = Watchdog::spawn(Duration::from_millis(budget_ms), Arc::clone(&hits));
        (dog, hits)
    }

    fn hits(counter: &Arc<AtomicUsize>) -> usize {
        counter.load(Ordering::SeqCst)
    }

    /// 予算を超えた実行は叩かれ、`disarm` がそれを申告する。
    #[test]
    fn an_overrunning_run_is_interrupted() {
        let (dog, counter) = watchdog(50);
        dog.arm();
        std::thread::sleep(Duration::from_millis(300));
        assert!(dog.disarm(), "予算超過が申告されていない");
        assert_eq!(hits(&counter), 1);
    }

    /// 間に合った実行は叩かれない。
    #[test]
    fn a_run_within_the_budget_is_left_alone() {
        let (dog, counter) = watchdog(1000);
        dog.arm();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!dog.disarm());
        assert_eq!(hits(&counter), 0);
    }

    /// 予算は実行ごとに引き直される。短い実行を積み重ねても叩かれない
    /// (累計は予算を超えるが、1 回あたりは超えていない)。
    #[test]
    fn each_run_gets_a_fresh_budget() {
        let (dog, counter) = watchdog(150);
        for _ in 0..6 {
            dog.arm();
            std::thread::sleep(Duration::from_millis(40));
            assert!(!dog.disarm());
        }
        assert_eq!(hits(&counter), 0);
    }

    /// 叩かれた実行の次は、また間に合う判定に戻る (`fired` が持ち越されない)。
    #[test]
    fn the_verdict_does_not_leak_into_the_next_run() {
        let (dog, counter) = watchdog(50);
        dog.arm();
        std::thread::sleep(Duration::from_millis(200));
        assert!(dog.disarm());

        dog.arm();
        std::thread::sleep(Duration::from_millis(10));
        assert!(!dog.disarm(), "前の実行の判定が持ち越されている");
        assert_eq!(hits(&counter), 1);
    }

    /// JS が動いていない間は締め切りが無いので、いくら待っても叩かれない。
    #[test]
    fn an_idle_watchdog_never_fires() {
        let (_dog, counter) = watchdog(50);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(hits(&counter), 0);
    }

    // ---- 本物の V8 ------------------------------------------------------------

    fn guarded_runtime(budget: Duration) -> (rustyscript::Runtime, Watchdog) {
        let mut runtime = rustyscript::Runtime::new(rustyscript::RuntimeOptions {
            timeout: budget,
            ..Default::default()
        })
        .expect("failed to init JS runtime");
        let handle = runtime.deno_runtime().v8_isolate().thread_safe_handle();
        let dog = Watchdog::spawn(budget, handle);
        (runtime, dog)
    }

    /// **#59 の完了条件。** 同期の無限ループを書いたトリガーは中断され、同じ Runtime に
    /// 載っている別のトリガーはその後も動く。
    ///
    /// 予算超過そのものより「Runtime が壊れずに次が動く」ほうが要点である。壊れるなら
    /// 中断だけでは足りず、Runtime の作り直しが要ることになる。
    #[test]
    fn a_spinning_trigger_does_not_stop_the_next_one() {
        let (mut runtime, dog) = guarded_runtime(Duration::from_millis(500));

        let spinner =
            rustyscript::Module::new("spin.js", "export function tick() { while (true) {} }");
        let spinner = runtime.load_module(&spinner).expect("load spinner");
        let neighbour = rustyscript::Module::new(
            "neighbour.js",
            "export function tick() { return 'まだ動きます'; }",
        );
        let neighbour = runtime.load_module(&neighbour).expect("load neighbour");

        let err = dog
            .guard(&mut runtime, |rt| {
                rt.call_function::<serde_json::Value>(
                    Some(&spinner),
                    "tick",
                    rustyscript::json_args!(),
                )
            })
            .expect_err("無限ループが中断されていない");
        assert!(err.contains("上限を超えた"), "{err}");

        let survived: String = dog
            .guard(&mut runtime, |rt| {
                rt.call_function(Some(&neighbour), "tick", rustyscript::json_args!())
            })
            .expect("隣のトリガーが動かない");
        assert_eq!(survived, "まだ動きます");
    }

    /// 非同期の待ちも同じ 1 文で中断される。こちらを拾うのは番犬ではなく
    /// `RuntimeOptions::timeout` だが、観測面からは区別しない。
    #[test]
    fn an_awaited_promise_that_never_resolves_is_also_cut() {
        let (mut runtime, dog) = guarded_runtime(Duration::from_millis(500));

        let hang = rustyscript::Module::new(
            "hang.js",
            "export async function tick() { await new Promise(() => {}); }",
        );
        let hang = runtime.load_module(&hang).expect("load hang");
        let neighbour = rustyscript::Module::new(
            "neighbour.js",
            "export function tick() { return 'まだ動きます'; }",
        );
        let neighbour = runtime.load_module(&neighbour).expect("load neighbour");

        let err = dog
            .guard(&mut runtime, |rt| {
                rt.call_function::<serde_json::Value>(
                    Some(&hang),
                    "tick",
                    rustyscript::json_args!(),
                )
            })
            .expect_err("解決しない await が中断されていない");
        assert!(err.contains("上限を超えた"), "{err}");

        let survived: String = dog
            .guard(&mut runtime, |rt| {
                rt.call_function(Some(&neighbour), "tick", rustyscript::json_args!())
            })
            .expect("隣のトリガーが動かない");
        assert_eq!(survived, "まだ動きます");
    }

    /// 予算内で失敗した JS のエラーは言い換えない。「中断された」と「JS が投げた」を
    /// 混ぜると、観測面から原因の切り分けができなくなる。
    #[test]
    fn an_ordinary_js_error_keeps_its_own_message() {
        let (mut runtime, dog) = guarded_runtime(Duration::from_secs(10));

        let boom = rustyscript::Module::new(
            "boom.js",
            "export function tick() { throw new Error('壊れています'); }",
        );
        let boom = runtime.load_module(&boom).expect("load boom");

        let err = dog
            .guard(&mut runtime, |rt| {
                rt.call_function::<serde_json::Value>(
                    Some(&boom),
                    "tick",
                    rustyscript::json_args!(),
                )
            })
            .expect_err("投げたのに成功している");
        assert!(err.contains("壊れています"), "{err}");
        assert!(!err.contains("上限を超えた"), "{err}");
    }
}

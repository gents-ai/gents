use regolith::env::opfs::{OpfsEnv, OpfsMode, OpfsOptions};
use regolith::env::{Env, WriteMode};
use regolith::{Db, Options};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    rc::Rc,
    task::{Poll, Waker},
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(module = "/tests/wasm_opfs_main/persist.js")]
extern "C" {
    #[wasm_bindgen(js_name = gateManifest)]
    fn gate_manifest() -> JsValue;
    #[wasm_bindgen(js_name = manifestPaused)]
    async fn manifest_paused(gate: &JsValue);
    #[wasm_bindgen(js_name = manifestClosed)]
    async fn manifest_closed(gate: &JsValue);
    #[wasm_bindgen(js_name = laterCalls)]
    fn later_calls(gate: &JsValue) -> u32;
    fn release(gate: &JsValue);
    fn restore(gate: &JsValue);
    #[wasm_bindgen(js_name = drainMicrotasks)]
    async fn drain_microtasks();
    #[wasm_bindgen(js_name = failRemovals)]
    fn fail_removals() -> JsValue;
    #[wasm_bindgen(js_name = allowRemovals)]
    fn allow_removals(fault: &JsValue);
    #[wasm_bindgen(js_name = removalAttempts)]
    fn removal_attempts(fault: &JsValue) -> u32;
    #[wasm_bindgen(catch, js_name = mirrorFileExists)]
    async fn mirror_file_exists(database: &str, path: &str) -> Result<JsValue, JsValue>;
}

struct Hook(JsValue);

impl Drop for Hook {
    fn drop(&mut self) {
        restore(&self.0);
    }
}

#[derive(Default)]
struct Cancel {
    requested: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

impl Cancel {
    fn cancel(&self) {
        self.requested.set(true);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

fn persist_task(env: OpfsEnv) -> (JsFuture, Rc<Cancel>) {
    let cancel = Rc::new(Cancel::default());
    let signal = cancel.clone();
    let promise = future_to_promise(async move {
        let mut pending = Box::pin(env.persist());
        std::future::poll_fn(|cx| {
            *signal.waker.borrow_mut() = Some(cx.waker().clone());
            if signal.requested.get() {
                Poll::Ready(Ok(()))
            } else {
                pending.as_mut().poll(cx)
            }
        })
        .await
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(JsValue::UNDEFINED)
    });
    (JsFuture::from(promise), cancel)
}

async fn mirror(name: &str) -> OpfsEnv {
    OpfsEnv::mount(
        name,
        OpfsOptions {
            force_mode: Some(OpfsMode::Mirror),
            ..OpfsOptions::default()
        },
    )
    .await
    .expect("mount mirror")
}

async fn persist_during_compaction(name: &str, cancel_first: bool) {
    let overtook = {
        let env = mirror(name).await;
        let db = Db::open(env.db_path(), Options::embedded().env(env.as_env())).unwrap();
        db.put(b"key", b"old").unwrap();
        db.flush().unwrap();

        let gate = Hook(gate_manifest());
        let (first, cancel) = persist_task(env.clone());
        let mut first = Some(first);
        manifest_paused(&gate.0).await;
        if cancel_first {
            cancel.cancel();
            first
                .take()
                .unwrap()
                .await
                .expect("drop the first caller's persist future");
        }

        db.put(b"key", b"new").unwrap();
        db.flush().unwrap();
        db.compact_range(None, None).wait().unwrap();
        let (second, _) = persist_task(env.clone());
        drain_microtasks().await;
        let overtook = later_calls(&gate.0) > 0;
        if overtook {
            // Let an overtaking batch finish so remount checks the resulting
            // durable files after the held manifest close.
            second.await.expect("persist replacement tables");
            release(&gate.0);
        } else {
            release(&gate.0);
            second.await.expect("persist the queued replacement");
        }
        if let Some(first) = first {
            first.await.expect("persist the first batch");
        }
        manifest_closed(&gate.0).await;
        assert_eq!(env.pending_bytes(), 0);
        db.close().unwrap();
        overtook
    };

    let env = mirror(name).await;
    let db = Db::open(env.db_path(), Options::embedded().env(env.as_env()))
        .expect("both acknowledged batches must leave a recoverable database");
    assert_eq!(db.get(b"key").unwrap().as_deref(), Some(&b"new"[..]));
    assert!(
        !overtook,
        "a newer persistence batch overtook the paused batch"
    );
    db.close().unwrap();
}

#[wasm_bindgen_test]
async fn overlapping_persist_cannot_publish_a_retired_manifest() {
    persist_during_compaction("regolith-main-overlapping-persist", false).await;
}

#[wasm_bindgen_test]
async fn dropping_a_persist_caller_keeps_its_browser_writes_serialized() {
    persist_during_compaction("regolith-main-cancelled-persist", true).await;
}

#[wasm_bindgen_test]
async fn failed_removals_remain_retryable() {
    let name = "regolith-main-removal-retry";
    let env = mirror(name).await;
    let path = env.db_path().join("obsolete");
    let mut file = env.open_write(&path, WriteMode::Truncate).unwrap();
    file.write_all(b"old table").unwrap();
    drop(file);
    env.persist().await.unwrap();
    env.remove_file(&path).unwrap();
    let fault = Hook(fail_removals());
    assert!(
        env.persist().await.is_err(),
        "a refused deletion must fail persistence"
    );
    assert_eq!(removal_attempts(&fault.0), 1);
    assert_eq!(
        mirror_file_exists(name, path.to_str().unwrap())
            .await
            .unwrap(),
        JsValue::TRUE
    );
    allow_removals(&fault.0);
    env.persist().await.expect("retry the retained removal");
    assert_eq!(removal_attempts(&fault.0), 2);
    assert_eq!(
        mirror_file_exists(name, path.to_str().unwrap())
            .await
            .unwrap(),
        JsValue::FALSE
    );
    // Removing a file that never reached OPFS must still succeed.
    drop(env.open_write(&path, WriteMode::Truncate).unwrap());
    env.remove_file(&path).unwrap();
    env.persist()
        .await
        .expect("an absent file is already removed");
}

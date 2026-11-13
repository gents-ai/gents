use std::cell::Cell;
use std::rc::Rc;

use rquickjs::{Context, Function, Promise, Runtime};
use serde_json::Value;

/// Why a host call returned no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// The invocation ended (cancelled, interrupted, timed out or expired).
    Stopped,
    Failed(String),
}

pub const CANCELLED: &str = "script cancelled: the host stopped this invocation";

/// QuickJS runs its interrupt handler once per 10,000 backward jumps and
/// calls. Every this many handler runs the guest asks the host whether the
/// invocation still stands: the host can stop a guest only inside a host
/// call, so a script that never makes one would otherwise run to its
/// wall-clock limit after a cancel. A probe costs about a millisecond on the
/// host (a zero sleep waits for its timer), so a tight loop probes about
/// every 70 ms and spends about 2% of its time doing so.
pub const PROBE_EVERY: u32 = 512;

const PROBE: &str = r#"{"sleep_ms":0}"#;

const API: &str = r#"
const invoke = async request => {
    const answer = JSON.parse(__gents_host(JSON.stringify(request)));
    if (Object.prototype.hasOwnProperty.call(answer, 'error')) throw new Error(answer.error);
    return answer.result;
};
const sleep = async milliseconds => {
    if (!Number.isSafeInteger(milliseconds) || milliseconds < 0)
        throw new TypeError('sleep expects a nonnegative integer number of milliseconds');
    return invoke({sleep_ms: milliseconds});
};
const tools = __tools_enabled ? new Proxy(Object.create(null), {
    get: (_, name) => name === 'then' ? undefined : async (args = {}) => {
        if (typeof name !== 'string' || args === null || Array.isArray(args) || typeof args !== 'object')
            throw new TypeError('a tool call expects an object of arguments');
        return invoke({tool_name: name, arguments: args});
    }
}) : undefined;
const input = __input;
"#;

/// Afterburner's bundled interpreter exposes a fixed host-function set.
/// This guest embeds QuickJS so its JavaScript can call the generic Gents import.
pub fn run<F>(request: &Value, host: F) -> Result<Value, String>
where
    F: Fn(String) -> Result<String, HostError> + 'static,
{
    let source = request["script"]
        .as_str()
        .ok_or("script must be a string")?;
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    let host = Rc::new(host);
    let stopped = Rc::new(Cell::new(false));
    let (probe, probe_stopped) = (host.clone(), stopped.clone());
    let mut checks = 0;
    runtime.set_interrupt_handler(Some(Box::new(move || {
        checks += 1;
        if !probe_stopped.get() && checks % PROBE_EVERY == 0 {
            probe_stopped.set(probe(PROBE.into()) == Err(HostError::Stopped));
        }
        probe_stopped.get()
    })));
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        let call_stopped = stopped.clone();
        let host = Function::new(ctx.clone(), move |request: String| {
            host(request).unwrap_or_else(|error| {
                let error = match error {
                    HostError::Stopped => {
                        call_stopped.set(true);
                        CANCELLED.into()
                    }
                    HostError::Failed(error) => error,
                };
                serde_json::json!({"error": error}).to_string()
            })
        }).map_err(|e| e.to_string())?;
        ctx.globals().set("__gents_host", host).map_err(|e| e.to_string())?;
        let enabled = request["tool_calls"] == true && request["with_tools"] != false;
        ctx.globals().set("__tools_enabled", enabled).map_err(|e| e.to_string())?;
        let input = ctx.json_parse(request.get("input").unwrap_or(&Value::Null).to_string())
            .map_err(|e| e.to_string())?;
        ctx.globals().set("__input", input).map_err(|e| e.to_string())?;
        let program = format!("(async () => {{ {API}\nreturn await (async () => {{\n{source}\n}})(); }})().then(value => JSON.stringify(value === undefined ? null : value))");
        let evaluate = || -> rquickjs::Result<String> {
            let promise: Promise = ctx.eval(program)?;
            // A script can catch the failed call and the interruption of a
            // function it awaited, so a stop is checked between jobs too.
            while !stopped.get() {
                if let Some(result) = promise.result() {
                    return result;
                }
                if !ctx.execute_pending_job() {
                    break;
                }
            }
            Err(rquickjs::Error::WouldBlock)
        };
        let output = evaluate().map_err(|error| {
            if stopped.get() {
                return CANCELLED.into();
            }
            if error.is_exception() {
                let exception = ctx.catch();
                if let Some(object) = exception.as_object() {
                    let message = object.get::<_, String>("message").unwrap_or_else(|_| error.to_string());
                    let stack = object.get::<_, String>("stack").unwrap_or_default();
                    return format!("{message}\n{stack}");
                }
            }
            error.to_string()
        })?;
        serde_json::from_str(&output).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    #[test]
    fn scratch_computation_needs_no_host() {
        assert_eq!(
            run(
                &json!({"script": "return input.reduce((a,b) => a+b, 0)", "input": [1,2,3]}),
                |_| panic!("unexpected host call")
            )
            .unwrap(),
            6
        );
    }

    #[test]
    fn script_errors_report_the_message() {
        for (script, message) in [
            ("throw new Error('specific failure')", "specific failure"),
            ("return (", "unexpected"),
        ] {
            let error = run(&json!({"script": script}), |_| {
                panic!("unexpected host call")
            })
            .unwrap_err();
            assert!(error.to_lowercase().contains(message), "{error}");
        }
    }

    #[test]
    fn locals_survive_awaits_and_tools_are_opt_in() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seen = calls.clone();
        let result = run(&json!({"tool_calls": true, "script": "let n=7; for(let i=0;i<3;i++) n += await tools.add({i}); return n;"}), move |request| {
            let request: Value = serde_json::from_str(&request).unwrap();
            seen.borrow_mut().push(request);
            Ok(json!({"result": 2}).to_string())
        }).unwrap();
        assert_eq!(result, 13);
        assert_eq!(calls.borrow().len(), 3);
        for request in [
            json!({"script": "return typeof tools"}),
            json!({"tool_calls": true, "with_tools": false, "script": "return typeof tools"}),
        ] {
            assert_eq!(
                run(&request, |_| panic!("unexpected host call")).unwrap(),
                "undefined"
            );
        }
    }

    #[test]
    fn errors_are_catchable_and_sleep_uses_the_host() {
        let result = run(&json!({"tool_calls": true, "script": "try { await tools.nope({}); } catch(e) { await sleep(1); return e.message; }"}), |request| {
            let request: Value = serde_json::from_str(&request).unwrap();
            Ok(if request.get("sleep_ms").is_some() { json!({"result": null}) } else { json!({"error": "denied"}) }.to_string())
        }).unwrap();
        assert_eq!(result, "denied");
    }

    #[test]
    fn one_script_waits_for_a_condition_then_applies_a_bulk_change() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seen = calls.clone();
        let answer = run(
            &json!({"tool_calls": true, "script": r#"
            while (!(await tools.query({})).ready) await sleep(1);
            for (const path of ['a', 'b', 'c']) await tools.write_file({path, content: 'done'});
            return {changed: 3};
        "#}),
            move |text| {
                let request: Value = serde_json::from_str(&text).unwrap();
                let mut calls = seen.borrow_mut();
                let result = if request["tool_name"] == "query" {
                    json!({"ready": calls.len() > 1})
                } else {
                    Value::Null
                };
                calls.push(request);
                Ok(json!({"result": result}).to_string())
            },
        )
        .unwrap();
        assert_eq!(answer, json!({"changed": 3}));
        let calls = calls.borrow();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[1], json!({"sleep_ms": 1}));
        assert_eq!(calls[5]["arguments"]["path"], "c");
    }

    /// Answers `healthy` host calls, then reports the invocation stopped.
    fn stops_after(
        healthy: usize,
    ) -> (
        Rc<RefCell<Vec<Value>>>,
        impl Fn(String) -> Result<String, HostError>,
    ) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let seen = calls.clone();
        (calls, move |request| {
            let mut calls = seen.borrow_mut();
            calls.push(serde_json::from_str(&request).unwrap());
            if calls.len() > healthy {
                Err(HostError::Stopped)
            } else {
                Ok(json!({"result": null}).to_string())
            }
        })
    }

    #[test]
    fn a_stopped_invocation_ends_a_script_that_never_calls_the_host() {
        for script in [
            "while (true) {}",
            "while (true) { try { while (true) {} } catch (e) {} finally { continue; } }",
            "while (true) { try { await (async () => { while (true) {} })(); } catch (e) {} }",
        ] {
            let (calls, host) = stops_after(3);
            let error = run(&json!({"script": script}), host).unwrap_err();
            assert_eq!(error, CANCELLED, "{script}");
            let calls = calls.borrow();
            assert_eq!(calls.len(), 4, "{script}");
            assert!(
                calls.iter().all(|call| call == &json!({"sleep_ms": 0})),
                "{script}"
            );
        }
    }

    #[test]
    fn a_stopped_invocation_ends_a_script_that_catches_the_failed_call() {
        for script in [
            "while (true) { try { await sleep(1); } catch (e) {} }",
            "try { await sleep(1); } catch (e) { return e.message; }",
        ] {
            let (calls, host) = stops_after(0);
            let error = run(&json!({"script": script}), host).unwrap_err();
            assert_eq!(error, CANCELLED, "{script}");
            assert_eq!(*calls.borrow(), [json!({"sleep_ms": 1})], "{script}");
        }
    }

    #[test]
    fn probes_are_rare_and_other_refusals_do_not_cancel() {
        let calls = Rc::new(Cell::new(0u32));
        let seen = calls.clone();
        let loops = 4 * PROBE_EVERY * 10_000;
        let answer = run(
            &json!({"script": format!("let n = 0; for (let i = 0; i < {loops}; i++) n++; return n;")}),
            move |_| {
                seen.set(seen.get() + 1);
                Err(HostError::Failed("refused".into()))
            },
        )
        .unwrap();
        assert_eq!(answer, loops);
        // QuickJS counts each backward jump and call, a few per iteration.
        assert!((4..=16).contains(&calls.get()), "{} probes", calls.get());
    }

    #[test]
    #[ignore = "timing: prints the cost of the interrupt checks on a tight loop"]
    fn measure_probe_overhead() {
        let script = "let n = 0; for (let i = 0; i < 100000000; i++) n++; return n;";
        for label in ["free host", "50us host"] {
            let calls = Rc::new(Cell::new(0u32));
            let seen = calls.clone();
            let slow = label != "free host";
            let started = std::time::Instant::now();
            run(&json!({"script": script}), move |_| {
                seen.set(seen.get() + 1);
                if slow {
                    std::thread::sleep(std::time::Duration::from_micros(50));
                }
                Ok(json!({"result": null}).to_string())
            })
            .unwrap();
            eprintln!("{label}: {:?}, {} probes", started.elapsed(), calls.get());
        }
    }
}

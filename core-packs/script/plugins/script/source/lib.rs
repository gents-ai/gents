use std::cell::Cell;
use std::rc::Rc;

use rquickjs::promise::PromiseState;
use rquickjs::{Context, Function, Promise, Runtime};
use serde_json::Value;

mod help;

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

/// Console lines come back beside the result, so past this many characters a
/// line is counted instead of kept: printing cannot push the result over the
/// plugin's output limit.
pub const CONSOLE_CHARS: usize = 64 * 1024;

const UNKNOWN_TOOL_HINT: &str = "If this was a tools.NAME call, NAME is not on this request's tool surface; Object.keys(tools) lists the names that are.";

const API: &str = r#"
const __failed = answer => Object.prototype.hasOwnProperty.call(answer, 'error');
const __ask = request => JSON.parse(__gents_host(JSON.stringify(request)));
const __protocol = (operation, answer) => {
    const error = new Error(`ScriptHostProtocolMismatch: ${operation}: ${JSON.stringify(answer)}. Update the host or install the script pack version matching it.`);
    error.name = 'ScriptHostProtocolMismatch';
    return error;
};
const __queue = [];
const invoke = request => new Promise((resolve, reject) => { __queue.push({request, resolve, reject}); });
Object.defineProperty(globalThis, '__gents_flush', {value: () => {
    if (__queue.length === 0) return false;
    const waiting = __queue.splice(0);
    const settle = (call, answer) => __failed(answer) ? call.reject(new Error(answer.error)) : call.resolve(answer.result);
    if (waiting.length === 1) {
        settle(waiting[0], __ask(waiting[0].request));
        return true;
    }
    const answer = __ask({batch: waiting.map(call => call.request)});
    if (__failed(answer) && !String(answer.error).startsWith('expected {tool_name, arguments} or {sleep_ms}')) {
        waiting.forEach(call => call.reject(new Error(answer.error)));
    } else if (!Array.isArray(answer.results) || answer.results.length !== waiting.length) {
        const error = __protocol('batch needs one result per member', answer);
        waiting.forEach(call => call.reject(error));
    } else {
        waiting.forEach((call, index) => settle(call, answer.results[index]));
    }
    return true;
}});
const __printed = {lines: [], room: __console_chars, omitted: 0};
const __shown = value => {
    if (typeof value === 'string') return value;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    try { return JSON.stringify(value) ?? String(value); } catch { return String(value); }
};
const __print = (...values) => {
    const line = values.map(__shown).join(' ');
    if (__printed.omitted > 0 || line.length > __printed.room) return void __printed.omitted++;
    __printed.lines.push(line);
    __printed.room -= line.length;
};
const console = Object.freeze({log: __print, info: __print, debug: __print, warn: __print, error: __print, dir: __print, table: __print, trace: __print});
Object.defineProperty(globalThis, '__gents_console', {value: () => __printed.omitted === 0
    ? __printed.lines
    : [...__printed.lines, `(${__printed.omitted} more console lines omitted)`]});
const sleep = async milliseconds => {
    if (!Number.isSafeInteger(milliseconds) || milliseconds < 0)
        throw new TypeError('sleep expects a nonnegative integer number of milliseconds');
    return invoke({sleep_ms: milliseconds});
};
const tools = __tools_enabled ? (() => {
    const described = request => {
        const answer = __ask({describe: request});
        const result = answer.result;
        const valid = request === null
            ? Array.isArray(result) && result.every(name => typeof name === 'string')
            : result && typeof result.description === 'string' && result.parameters && typeof result.parameters === 'object';
        if (!valid) throw __protocol('describe returned no tool surface', answer);
        return result;
    };
    let names;
    const known = name => typeof name === 'string' && name !== 'then' && (names ??= described(null)).includes(name);
    const callable = new Map();
    const tool = name => {
        if (!callable.has(name)) {
            let definition;
            const describe = () => definition ??= described(name);
            const call = async (args = {}) => {
                if (args === null || Array.isArray(args) || typeof args !== 'object')
                    throw new TypeError('a tool call expects an object of arguments');
                return invoke({tool_name: name, arguments: args});
            };
            Object.defineProperties(call, {
                name: {value: name},
                description: {get: () => describe().description, enumerable: true},
                parameters: {get: () => describe().parameters, enumerable: true},
            });
            callable.set(name, call);
        }
        return callable.get(name);
    };
    return new Proxy(Object.create(null), {
        get: (_, name) => known(name) ? tool(name) : undefined,
        has: (_, name) => known(name),
        ownKeys: () => (names ??= described(null)).slice(),
        getOwnPropertyDescriptor: (_, name) => known(name)
            ? {value: tool(name), enumerable: true, configurable: true, writable: false} : undefined,
    });
})() : undefined;
const __tool = name => {
    if (tools !== undefined && name in tools) return tools[name];
    throw new Error(`wait needs tools.${name}, which this request does not grant; Object.keys(tools) lists the tools it does`);
};
const wait = async (run, {timeout_ms = 60000} = {}) => {
    if (run === null || typeof run !== 'object' || (typeof run.session_id !== 'string' && typeof run.tool_call_id !== 'string'))
        throw new TypeError('wait takes the receipt that agent_new, agent_message, config task run or spawn_process returned');
    if (!Number.isSafeInteger(timeout_ms) || timeout_ms < 0)
        throw new TypeError('wait expects timeout_ms as a nonnegative integer number of milliseconds');
    if (typeof run.session_id !== 'string')
        return __tool('wait_process')({tool_call_id: run.tool_call_id, timeout_secs: Math.max(1, Math.ceil(timeout_ms / 1000))});
    const read = __tool('agent_session_read');
    const later = `agent_session_read({session_id: ${JSON.stringify(run.session_id)}, view: "last"})`;
    let waited = 0;
    for (let pause = 500; ; pause = Math.min(pause * 2, 10000)) {
        const seen = await read({session_id: run.session_id, view: 'last'});
        const request = seen.request ?? {};
        if (request.terminalized_at && (run.request_id == null || request.request_id === run.request_id)) {
            const message = seen.messages?.items?.[0] ?? {};
            let reply = message.text ?? null;
            try { reply = JSON.parse(reply); } catch {}
            return {reply, reply_truncated: message.text_truncated === true, ...request, session_id: run.session_id};
        }
        const state = request.lifecycle_state ?? 'not started';
        const step = Math.min(pause, timeout_ms - waited);
        if (step <= 0)
            throw new Error(`the run is still ${state} after ${timeout_ms} ms; call wait again with a larger timeout_ms, or return and read it later with ${later}`);
        try { await sleep(step); }
        catch (error) { throw new Error(`${error.message}; the run is still ${state}: return and read it later with ${later}`); }
        waited += step;
    }
};
const input = __input;
"#;

/// Afterburner's bundled interpreter exposes a fixed host-function set.
/// This guest embeds QuickJS so its JavaScript can call the generic Gents import.
pub fn run<F>(request: &Value, host: F) -> Result<Value, String>
where
    F: Fn(String) -> Result<String, HostError> + 'static,
{
    if let Some(topic) = request["help"].as_str() {
        return help::answer(topic);
    }
    let source = request["script"]
        .as_str()
        .ok_or(r#"script must be a JavaScript string; {"help": "topics"} lists the help topics"#)?;
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
        ctx.globals().set("__console_chars", CONSOLE_CHARS).map_err(|e| e.to_string())?;
        let input = ctx.json_parse(request.get("input").unwrap_or(&Value::Null).to_string())
            .map_err(|e| e.to_string())?;
        ctx.globals().set("__input", input).map_err(|e| e.to_string())?;
        let program = format!("(async () => {{ {API}\nreturn await (async () => {{\n{source}\n}})(); }})().then(value => {{ const result = value === undefined ? null : value; const console = __gents_console(); return JSON.stringify(console.length === 0 ? result : {{result, console}}); }})");
        let printed = || -> Vec<String> {
            ctx.globals()
                .get::<_, Function>("__gents_console")
                .and_then(|printed| printed.call(()))
                .unwrap_or_default()
        };
        // Host calls made while the job queue drains wait in the guest and go
        // to the host together once it is empty, so calls a script starts
        // before awaiting any of them reach the host as one batch.
        let flush = || -> rquickjs::Result<bool> {
            ctx.globals().get::<_, Function>("__gents_flush")?.call(())
        };
        let evaluate = || -> rquickjs::Result<String> {
            let promise: Promise = ctx.eval(program)?;
            // A script can catch the failed call and the interruption of a
            // function it awaited, so a stop is checked between jobs too.
            while !stopped.get() {
                if promise.state() != PromiseState::Pending {
                    flush()?;
                    if stopped.get() {
                        break;
                    }
                    return promise.result().unwrap_or(Err(rquickjs::Error::WouldBlock));
                }
                if !ctx.execute_pending_job() && !flush()? {
                    break;
                }
            }
            Err(rquickjs::Error::WouldBlock)
        };
        let output = evaluate().map_err(|error| {
            if stopped.get() {
                return CANCELLED.into();
            }
            let mut text = error.to_string();
            let mut hint = None;
            if error.is_exception() {
                let exception = ctx.catch();
                if let Some(object) = exception.as_object() {
                    let message = object.get::<_, String>("message").unwrap_or_else(|_| error.to_string());
                    let stack = object.get::<_, String>("stack").unwrap_or_default();
                    if enabled && message == "not a function" {
                        hint = Some(UNKNOWN_TOOL_HINT);
                    }
                    text = format!("{message}\n{stack}");
                }
            }
            let printed = printed();
            if !printed.is_empty() {
                text = format!("{text}\nconsole:\n{}", printed.join("\n"));
            }
            match hint {
                Some(hint) => format!("{text}\n{hint}"),
                None => text,
            }
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

    type Calls = Rc<RefCell<Vec<Value>>>;

    /// A host whose surface is `names`. It answers `describe` itself, records
    /// every other request as sent, and answers each call, alone or as a
    /// member of a batch, with `answer`.
    fn surface(
        names: &'static [&'static str],
        answer: impl Fn(&Value, usize) -> Value + 'static,
    ) -> (Calls, impl Fn(String) -> Result<String, HostError>) {
        let calls = Calls::default();
        let seen = calls.clone();
        (calls, move |request| {
            let request: Value = serde_json::from_str(&request).unwrap();
            let answered = match request.get("describe") {
                Some(Value::Null) => json!({"result": names}),
                Some(name) => json!({"result": {
                    "name": name,
                    "description": format!("describes {}", name.as_str().unwrap()),
                    "parameters": {"type": "object"},
                }}),
                None => {
                    let before = seen.borrow().len();
                    seen.borrow_mut().push(request.clone());
                    match request.get("batch").and_then(Value::as_array) {
                        Some(members) => json!({"results": members
                            .iter()
                            .map(|member| answer(member, before))
                            .collect::<Vec<_>>()}),
                        None => answer(&request, before),
                    }
                }
            };
            Ok(answered.to_string())
        })
    }

    #[test]
    fn locals_survive_awaits_and_tools_are_opt_in() {
        let (calls, host) = surface(&["add"], |_, _| json!({"result": 2}));
        let result = run(&json!({"tool_calls": true, "script": "let n=7; for(let i=0;i<3;i++) n += await tools.add({i}); return n;"}), host).unwrap();
        assert_eq!(result, 13);
        assert_eq!(
            *calls.borrow(),
            [0, 1, 2].map(|i| json!({"tool_name": "add", "arguments": {"i": i}}))
        );
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
        let (calls, host) = surface(&["refused"], |request, _| {
            if request.get("sleep_ms").is_some() {
                json!({"result": null})
            } else {
                json!({"error": "denied"})
            }
        });
        let result = run(&json!({"tool_calls": true, "script": "try { await tools.refused({}); } catch(e) { await sleep(1); return e.message; }"}), host).unwrap();
        assert_eq!(result, "denied");
        assert_eq!(calls.borrow()[1], json!({"sleep_ms": 1}));
    }

    #[test]
    fn one_script_waits_for_a_condition_then_applies_a_bulk_change() {
        let (calls, host) = surface(&["query", "write_file"], |request, before| {
            let result = if request["tool_name"] == "query" {
                json!({"ready": before > 1})
            } else {
                Value::Null
            };
            json!({"result": result})
        });
        let answer = run(
            &json!({"tool_calls": true, "script": r#"
            while (!(await tools.query({})).ready) await sleep(1);
            for (const path of ['a', 'b', 'c']) await tools.write_file({path, content: 'done'});
            return {changed: 3};
        "#}),
            host,
        )
        .unwrap();
        assert_eq!(answer, json!({"changed": 3}));
        let calls = calls.borrow();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[1], json!({"sleep_ms": 1}));
        assert_eq!(calls[5]["arguments"]["path"], "c");
    }

    #[test]
    fn calls_started_together_reach_the_host_as_one_batch() {
        let (calls, host) = surface(&["read", "write"], |request, _| {
            match request["arguments"]["path"].as_str() {
                Some("missing") => json!({"error": "no such file"}),
                Some(path) => json!({"result": format!("contents of {path}")}),
                None => json!({"result": null}),
            }
        });
        let answer = run(
            &json!({"tool_calls": true, "script": r#"
            const [a, b] = await Promise.all(['a', 'b'].map(path => tools.read({path})));
            const settled = await Promise.allSettled([tools.read({path: 'missing'}), tools.read({path: 'c'}), sleep(5)]);
            const alone = await tools.read({path: 'd'});
            return {a, b, alone, settled: settled.map(s => s.status === 'fulfilled' ? s.value : s.reason.message)};
        "#}),
            host,
        )
        .unwrap();
        assert_eq!(
            answer,
            json!({"a": "contents of a", "b": "contents of b", "alone": "contents of d",
                "settled": ["no such file", "contents of c", null]})
        );
        let read = |path: &str| json!({"tool_name": "read", "arguments": {"path": path}});
        assert_eq!(
            *calls.borrow(),
            [
                json!({"batch": [read("a"), read("b")]}),
                json!({"batch": [read("missing"), read("c"), {"sleep_ms": 5}]}),
                read("d"),
            ]
        );
    }

    #[test]
    fn an_old_host_or_malformed_batch_fails_with_a_named_protocol_error() {
        let error = run(
            &json!({"tool_calls": true, "script": "return Object.keys(tools);"}),
            |_| Ok(json!({"error": "expected {tool_name, arguments} or {sleep_ms}"}).to_string()),
        )
        .unwrap_err();
        assert!(
            error.contains("ScriptHostProtocolMismatch: describe"),
            "{error}"
        );
        for reply in [
            json!({"error": "expected {tool_name, arguments} or {sleep_ms}"}),
            json!({"results": []}),
        ] {
            let error = run(
                &json!({"script": "await Promise.all([sleep(0), sleep(0)]);"}),
                move |_| Ok(reply.to_string()),
            )
            .unwrap_err();
            assert!(
                error.contains("ScriptHostProtocolMismatch: batch"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_batch_host_error_preserves_the_hosts_own_diagnostic() {
        let error = run(
            &json!({"script": "await Promise.all([sleep(0), sleep(0)]);"}),
            |_| Ok(json!({"error": "TOO_LARGE: host request exceeds 1 MiB"}).to_string()),
        )
        .unwrap_err();
        assert!(error.contains("TOO_LARGE"), "{error}");
        assert!(!error.contains("ScriptHostProtocolMismatch"), "{error}");
    }

    #[test]
    fn a_call_the_script_never_awaits_still_reaches_the_host() {
        for script in [
            "tools.write({path: 'a'}); return 1;",
            "tools.write({path: 'a'}); throw new Error('after the call');",
        ] {
            let (calls, host) = surface(&["write"], |_, _| json!({"result": null}));
            let outcome = run(&json!({"tool_calls": true, "script": script}), host);
            assert_eq!(
                *calls.borrow(),
                [json!({"tool_name": "write", "arguments": {"path": "a"}})],
                "{script}"
            );
            match outcome {
                Ok(value) => assert_eq!(value, 1, "{script}"),
                Err(error) => assert!(error.contains("after the call"), "{error}"),
            }
        }
    }

    #[test]
    fn the_surface_is_listed_and_described_without_spending_a_call() {
        let (calls, host) = surface(&["query", "write_file"], |_, _| {
            panic!("introspection made a tool call")
        });
        let answer = run(
            &json!({"tool_calls": true, "script": r#"
            return {
                names: Object.keys(tools),
                has: ['query' in tools, 'nope' in tools],
                types: [typeof tools.query, typeof tools.nope],
                same: tools.query === tools.query,
                name: tools.write_file.name,
                description: tools.write_file.description,
                parameters: tools.write_file.parameters,
                awaited: (await tools) === tools,
            };
        "#}),
            host,
        )
        .unwrap();
        assert_eq!(
            answer,
            json!({
                "names": ["query", "write_file"],
                "has": [true, false],
                "types": ["function", "undefined"],
                "same": true,
                "name": "write_file",
                "description": "describes write_file",
                "parameters": {"type": "object"},
                "awaited": true,
            })
        );
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn calling_a_name_off_the_surface_says_how_to_list_it() {
        let (calls, host) = surface(&["query"], |_, _| panic!("unexpected tool call"));
        let error = run(
            &json!({"tool_calls": true, "script": "return await tools.nope({});"}),
            host,
        )
        .unwrap_err();
        assert!(error.contains("not a function"), "{error}");
        assert!(error.ends_with(UNKNOWN_TOOL_HINT), "{error}");
        assert!(calls.borrow().is_empty());
        let error = run(&json!({"script": "return undefined();"}), |_| {
            panic!("unexpected host call")
        })
        .unwrap_err();
        assert!(!error.contains(UNKNOWN_TOOL_HINT), "{error}");
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

use rquickjs::{Context, Function, Promise, Runtime};
use serde_json::Value;

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

pub fn run<F>(request: &Value, host: F) -> Result<Value, String>
where
    F: Fn(String) -> Result<String, String> + 'static,
{
    let source = request["script"]
        .as_str()
        .ok_or("script must be a string")?;
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        let host = Function::new(ctx.clone(), move |request: String| {
            host(request).unwrap_or_else(|error| serde_json::json!({"error": error}).to_string())
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
            promise.finish()
        };
        let output = evaluate().map_err(|error| {
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
    use std::rc::Rc;

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
}

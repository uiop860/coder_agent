use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc::{self, SyncSender};

use deno_ast::{EmitOptions, MediaType, ParseParams, TranspileModuleOptions, TranspileOptions};

// ── TypeScript → JavaScript transpilation ─────────────────────────────────────

/// Transpile TypeScript to JavaScript.
///
/// The user's code is wrapped in `function __agent_run__() { … }` before
/// being handed to deno_ast so that top-level `return` statements are valid
/// TypeScript (they're inside a function body).  The returned JS still
/// contains that wrapper; callers must invoke `__agent_run__()` themselves.
pub fn transpile_ts(code: &str) -> Result<String, String> {
    // Wrap in a named function so `return` is syntactically valid.
    let wrapped = format!("function __agent_run__() {{\n{}\n}}", code);

    let parsed = deno_ast::parse_module(ParseParams {
        specifier: deno_ast::ModuleSpecifier::parse("file:///agent_script.ts")
            .expect("valid specifier"),
        media_type: MediaType::TypeScript,
        text: Arc::from(wrapped.as_str()),
        capture_tokens: false,
        maybe_syntax: None,
        scope_analysis: false,
    })
    .map_err(|e| format!("TS parse error: {e}"))?;

    let transpiled = parsed
        .transpile(
            &TranspileOptions::default(),
            &TranspileModuleOptions::default(),
            &EmitOptions::default(),
        )
        .map_err(|e| format!("TS transpile error: {e}"))?;

    let source = transpiled.into_source();
    Ok(source.text.to_string())
}

// ── V8 executor running on a dedicated thread ────────────────────────────────

enum ExecutorMsg {
    Run {
        code: String,
        reply: SyncSender<Result<String, String>>,
    },
    Shutdown,
}

/// A Send handle to a V8 isolate running on its own OS thread.
///
/// Cheap to clone — each clone sends tasks to the same underlying isolate thread.
#[derive(Clone)]
pub struct JsExecutorHandle {
    tx: SyncSender<ExecutorMsg>,
}

impl JsExecutorHandle {
    /// Spawn the V8 isolate thread and return a handle.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::sync_channel::<ExecutorMsg>(1);

        std::thread::spawn(move || {
            // Create a fresh isolate
            let mut isolate = v8::Isolate::new(v8::CreateParams::default());

            // Process messages until shutdown
            while let Ok(msg) = rx.recv() {
                match msg {
                    ExecutorMsg::Run { code, reply } => {
                        let result = run_in_isolate(&mut isolate, &code);
                        let _ = reply.send(result);
                    }
                    ExecutorMsg::Shutdown => break,
                }
            }
        });

        Self { tx }
    }

    /// Transpile TypeScript to JavaScript, then execute it and return the result.
    ///
    /// `transpile_ts` wraps the code in `function __agent_run__() { … }`.  We
    /// append a call to that function so V8 evaluates its return value as the
    /// last expression (no IIFE needed).
    pub fn run_ts(&self, ts_code: &str) -> Result<String, String> {
        let js_code = transpile_ts(ts_code)?;
        // Append the call so V8's script.run() returns the function's value.
        let callable = format!("{js_code}\n__agent_run__();");

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(ExecutorMsg::Run {
                code: callable,
                reply: reply_tx,
            })
            .map_err(|_| "executor thread is gone".to_string())?;

        reply_rx
            .recv()
            .map_err(|_| "executor thread dropped reply".to_string())?
    }

    /// Execute raw JavaScript and return the stringified `__result__` value.
    pub fn run_js(&self, js_code: &str) -> Result<String, String> {
        // Wrap in an IIFE so the user code's return value is captured
        let wrapped = format!("const __result__ = (() => {{ {js_code} }})(); __result__;");

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(ExecutorMsg::Run {
                code: wrapped,
                reply: reply_tx,
            })
            .map_err(|_| "executor thread is gone".to_string())?;

        reply_rx
            .recv()
            .map_err(|_| "executor thread dropped reply".to_string())?
    }

    /// Shut down the V8 isolate thread.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        let _ = self.tx.send(ExecutorMsg::Shutdown);
    }
}

// ── Run code inside an existing isolate ──────────────────────────────────────

fn run_in_isolate(isolate: &mut v8::OwnedIsolate, code: &str) -> Result<String, String> {
    let handle_scope = &mut v8::HandleScope::new(isolate);

    // Create a new context with host functions on its global object
    let global_template = v8::ObjectTemplate::new(handle_scope);

    // Register readFile(path) -> string
    {
        let name = v8::String::new(handle_scope, "readFile").unwrap();
        let tmpl = v8::FunctionTemplate::new(handle_scope, read_file_callback);
        global_template.set(name.into(), tmpl.into());
    }

    // Register writeFile(path, content) -> undefined
    {
        let name = v8::String::new(handle_scope, "writeFile").unwrap();
        let tmpl = v8::FunctionTemplate::new(handle_scope, write_file_callback);
        global_template.set(name.into(), tmpl.into());
    }

    // Register print(...args) -> undefined
    {
        let name = v8::String::new(handle_scope, "print").unwrap();
        let tmpl = v8::FunctionTemplate::new(handle_scope, print_callback);
        global_template.set(name.into(), tmpl.into());
    }

    let context = v8::Context::new(
        handle_scope,
        v8::ContextOptions {
            global_template: Some(global_template),
            ..Default::default()
        },
    );
    let scope = &mut v8::ContextScope::new(handle_scope, context);

    // Compile
    let source = v8::String::new(scope, code).ok_or("failed to create V8 string")?;
    let mut try_catch = v8::TryCatch::new(scope);

    let script = match v8::Script::compile(&mut try_catch, source, None) {
        Some(s) => s,
        None => {
            let ex = try_catch.exception().unwrap();
            let msg = ex.to_rust_string_lossy(&mut try_catch);
            return Err(format!("Compile error: {msg}"));
        }
    };

    // Run
    match script.run(&mut try_catch) {
        Some(value) => {
            let result_str = value.to_rust_string_lossy(&mut try_catch);
            Ok(result_str)
        }
        None => {
            let ex = try_catch.exception().unwrap();
            let msg = ex.to_rust_string_lossy(&mut try_catch);
            Err(format!("Runtime error: {msg}"))
        }
    }
}

// ── Host function callbacks ──────────────────────────────────────────────────

// Base directory for sandboxed I/O; set once when first needed.
static BASE_DIR: OnceLock<PathBuf> = OnceLock::new();

fn ensure_within_base(path: &Path) -> Result<PathBuf, String> {
    let base = BASE_DIR.get_or_init(|| {
        std::env::current_dir()
            .expect("could not read cwd")
            .canonicalize()
            .expect("could not canonicalize cwd")
    });

    // Compute an absolute path rooted at `base` when the provided path is
    // relative.  We avoid calling `canonicalize` on the final path because
    // that would fail for non-existent files (which is exactly the case when
    // the agent wants to create a new file).  Instead we perform simple
    // component normalization to strip `.`/`..` and then verify the resulting
    // location still sits under `base`.
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };

    // Normalize the path components ourselves.  This will not resolve
    // symlinks but that's fine for our sandbox purposes.
    let mut comps = Vec::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::ParentDir => {
                comps.pop();
            }
            std::path::Component::CurDir => {
                // skip
            }
            other => comps.push(other.as_os_str().to_os_string()),
        }
    }
    let canon: PathBuf = comps.iter().collect();

    if !canon.starts_with(base) {
        Err(format!("path {canon:?} outside of base {base:?}"))
    } else {
        Ok(canon)
    }
}

fn read_file_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let path_str = args.get(0).to_rust_string_lossy(scope);
    let path = Path::new(&path_str);

    match ensure_within_base(path) {
        Ok(valid) => match std::fs::read_to_string(&valid) {
            Ok(contents) => {
                let v8_str = v8::String::new(scope, &contents).unwrap();
                rv.set(v8_str.into());
            }
            Err(e) => {
                let msg = v8::String::new(scope, &format!("readFile error: {e}")).unwrap();
                let exception = v8::Exception::error(scope, msg);
                scope.throw_exception(exception);
            }
        },
        Err(err) => {
            let msg =
                v8::String::new(scope, &format!("readFile sandbox violation: {err}")).unwrap();
            let exception = v8::Exception::error(scope, msg);
            scope.throw_exception(exception);
        }
    }
}

fn write_file_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let path_str = args.get(0).to_rust_string_lossy(scope);
    let content = args.get(1).to_rust_string_lossy(scope);
    let path = Path::new(&path_str);

    match ensure_within_base(path) {
        Ok(valid) => {
            if let Err(e) = std::fs::write(&valid, &content) {
                let msg = v8::String::new(scope, &format!("writeFile error: {e}")).unwrap();
                let exception = v8::Exception::error(scope, msg);
                scope.throw_exception(exception);
            }
        }
        Err(err) => {
            let msg =
                v8::String::new(scope, &format!("writeFile sandbox violation: {err}")).unwrap();
            let exception = v8::Exception::error(scope, msg);
            scope.throw_exception(exception);
        }
    }
}

fn print_callback(
    scope: &mut v8::HandleScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let mut parts = Vec::new();
    for i in 0..args.length() {
        parts.push(args.get(i).to_rust_string_lossy(scope));
    }
    eprintln!("[js] {}", parts.join(" "));
}

// ── Tests for sandboxing helper ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_ensure_within_base_rejects_parent() {
        // Trigger initialization
        let _ = BASE_DIR.get_or_init(|| {
            std::env::current_dir()
                .expect("cwd")
                .canonicalize()
                .expect("canon")
        });
        // A path pointing to parent directory should be rejected
        let res = ensure_within_base(Path::new(".."));
        assert!(res.is_err(), "parent directory must be disallowed");
    }

    #[test]
    fn test_ensure_within_base_accepts_self() {
        let res = ensure_within_base(Path::new("."));
        assert!(res.is_ok());
    }
}

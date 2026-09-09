//! Minimal Numax Python guest, powered by RustPython.
//!
//! Compiled to a *core* WebAssembly module (wasm32-wasip1), not a
//! Component, so it loads via `wasmtime::Module` the same way the C,
//! C++, and TinyGo guest examples do.

use rustpython_vm::compiler::Mode;
use rustpython_vm::Interpreter;

mod nx_ffi {
    // import `host_log_v2` from the `nx` namespace.
    // strings are passed as: (pointer, length)
    // signature: (u32, u32) -> i32
    //
    // import `db_set` from the `nx` namespace.
    // writes a key/value pair into the embedded datastore.
    // signature: (u32, u32, u32, u32) -> i32
    #[link(wasm_import_module = "nx")]
    extern "C" {
        pub fn host_log_v2(ptr: *const u8, len: u32) -> i32;
        pub fn db_set(
            key_ptr: *const u8,
            key_len: u32,
            val_ptr: *const u8,
            val_len: u32,
        ) -> i32;
    }
}

/// The `nx` module as seen *inside* Python. Keeps the Python-facing API
/// (`nx.log(...)`, `nx.db_set(...)`) separate from the raw pointer/length
/// ABI used to cross the WASM boundary — C and C++ call the host
/// functions directly since they have no namespace concept; Python needs
/// something to hang `log`/`db_set` off of, so it's exposed as `nx.*`.
#[rustpython_vm::pymodule]
mod nx {
    use crate::nx_ffi;
    use rustpython_vm::builtins::PyStrRef;
    use rustpython_vm::{PyResult, VirtualMachine};

    #[pyfunction]
    fn log(msg: PyStrRef, vm: &VirtualMachine) -> PyResult<()> {
        let bytes = msg.as_str().as_bytes();
        let rc = unsafe { nx_ffi::host_log_v2(bytes.as_ptr(), bytes.len() as u32) };
        if rc < 0 {
            return Err(vm.new_runtime_error(format!("nx.log failed (code {rc})")));
        }
        Ok(())
    }

    #[pyfunction]
    fn db_set(key: PyStrRef, value: PyStrRef, vm: &VirtualMachine) -> PyResult<i32> {
        let k = key.as_str().as_bytes();
        let v = value.as_str().as_bytes();
        let rc = unsafe { nx_ffi::db_set(k.as_ptr(), k.len() as u32, v.as_ptr(), v.len() as u32) };
        if rc < 0 {
            return Err(vm.new_runtime_error(format!("nx.db_set failed (code {rc})")));
        }
        Ok(rc)
    }
}

/// exported guest entrypoint expected by Numax.
/// Mirrors `func run()` in TinyGo and `void run()` in C/C++.
#[no_mangle]
pub extern "C" fn run() {
    let interp = Interpreter::without_stdlib(Default::default());

    interp.enter(|vm| {
        let scope = vm.new_scope_with_builtins();

        let nx_module = nx::make_module(vm);
        if let Err(exc) = scope.globals.set_item("nx", nx_module, vm) {
            vm.print_exception(exc);
            return;
        }

        let source = include_str!("guest.py");
        let code_obj = match vm.compile(source, Mode::Exec, "<guest.py>".to_owned()) {
            Ok(code) => code,
            Err(err) => {
                let exc = vm.new_syntax_error(&err, Some(source));
                vm.print_exception(exc);
                return;
            }
        };

        if let Err(exc) = vm.run_code_obj(code_obj, scope) {
            vm.print_exception(exc);
        }
    });
}

fn main() {}
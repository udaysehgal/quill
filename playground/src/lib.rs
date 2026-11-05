//! The quill compiler as a WebAssembly module, for the browser playground.
//!
//! A tiny C-style interface, so the page needs no JavaScript glue library:
//! JS copies the source into memory from `alloc`, calls `compile`, then
//! reads the text at `output_ptr()` / `output_len()`.

use std::cell::RefCell;

thread_local! {
    static OUTPUT: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Room for `len` bytes of input; `compile` frees it.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Compile the UTF-8 source at `ptr`. `mode` 0 = assembly, 1 = expanded
/// core forms. Returns 1 on success, 0 if the output is an error message.
///
/// # Safety
/// `ptr` must come from `alloc(len)` and hold `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compile(ptr: *mut u8, len: usize, mode: u32) -> u32 {
    let bytes = unsafe { Vec::from_raw_parts(ptr, len, len) };
    let src = String::from_utf8_lossy(&bytes);
    let result = if mode == 1 {
        quill::expand_source(&src, "program.scm")
    } else {
        quill::compile_source(&src, "program.scm")
    };
    let ok = result.is_ok();
    OUTPUT.with(|o| *o.borrow_mut() = result.unwrap_or_else(|e| e));
    ok as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn output_ptr() -> *const u8 {
    OUTPUT.with(|o| o.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn output_len() -> usize {
    OUTPUT.with(|o| o.borrow().len())
}

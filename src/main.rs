//! schemec — a Scheme-to-ARM64 compiler, built incrementally.
//!
//! Pipeline:  source text --(reader)--> Expr --(compiler)--> ARM64 assembly
//!            --(clang + runtime.c)--> native executable

mod compiler;
mod reader;

use std::process::Command;
use std::{env, fs};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: schemec <file.scm> [-o output] [-S]");
        std::process::exit(1);
    }
    let input = &args[1];
    let output = args
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "a.out".into());
    let asm_only = args.iter().any(|a| a == "-S");

    let src = fs::read_to_string(input).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
    let forms = reader::read_all(&src).unwrap_or_else(|e| fail(&format!("parse error: {e}")));
    let asm =
        compiler::compile_program(&forms).unwrap_or_else(|e| fail(&format!("compile error: {e}")));

    if asm_only {
        print!("{asm}");
        return;
    }

    let asm_path = format!("{output}.s");
    fs::write(&asm_path, asm).unwrap();

    let runtime = concat!(env!("CARGO_MANIFEST_DIR"), "/runtime/runtime.c");
    let status = Command::new("clang")
        .args([runtime, &asm_path, "-o", &output])
        .status()
        .expect("failed to run clang");
    if !status.success() {
        fail("clang failed");
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("schemec: {msg}");
    std::process::exit(1);
}

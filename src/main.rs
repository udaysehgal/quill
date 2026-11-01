//! A Scheme-to-ARM64 compiler.
//!
//! Pipeline: source text --(reader)--> Expr --(expander)--> core forms
//!           --(compiler)--> ARM64 assembly --(clang + runtime.c)--> executable

mod compiler;
mod expand;
mod reader;
mod syntax;

use std::process::Command;
use std::{env, fs};

const NAME: &str = env!("CARGO_PKG_NAME");

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        eprintln!("usage: {NAME} <file.scm> [-o output] [-S]");
        eprintln!("  -o output   name of the executable (default a.out)");
        eprintln!("  -S          print the generated assembly instead");
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
    let forms = reader::read_all(&src).unwrap_or_else(|e| fail(&locate(input, &e)));
    let asm = compiler::compile_program(&forms, input).unwrap_or_else(|e| fail(&locate(input, &e)));

    if asm_only {
        print!("{asm}");
        return;
    }

    // assemble and link in a scratch directory, with the prebuilt runtime
    let work = env::temp_dir().join(format!("{NAME}-{}", std::process::id()));
    fs::create_dir_all(&work).unwrap_or_else(|e| fail(&format!("{}: {e}", work.display())));
    let asm_path = work.join("program.s");
    let runtime_path = work.join("runtime.o");
    fs::write(&asm_path, asm).unwrap_or_else(|e| fail(&format!("{}: {e}", asm_path.display())));
    fs::write(&runtime_path, RUNTIME_OBJECT)
        .unwrap_or_else(|e| fail(&format!("{}: {e}", runtime_path.display())));
    let status = Command::new("clang")
        .arg(&asm_path)
        .arg(&runtime_path)
        .args(["-o", &output])
        .status()
        .unwrap_or_else(|e| fail(&format!("couldn't run clang: {e}")));
    let _ = fs::remove_dir_all(&work);
    if !status.success() {
        fail("clang failed");
    }
}

/// runtime/runtime.c, compiled by build.rs.
const RUNTIME_OBJECT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime.o"));

/// `line 3: msg` -> `file.scm:3: msg`
fn locate(file: &str, err: &str) -> String {
    if let Some(rest) = err.strip_prefix("line ")
        && let Some((n, msg)) = rest.split_once(": ")
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return format!("{file}:{n}: {msg}");
    }
    format!("{file}: {err}")
}

fn fail(msg: &str) -> ! {
    eprintln!("{NAME}: {msg}");
    std::process::exit(1);
}

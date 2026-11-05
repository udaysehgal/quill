//! The quill command: compile a Scheme file with the library, then assemble
//! and link the result with clang and the prebuilt runtime.

use std::process::Command;
use std::{env, fs};

const NAME: &str = env!("CARGO_PKG_NAME");

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        eprintln!("usage: {NAME} <file.scm> [-o output] [-S]");
        eprintln!("  -o output   name of the executable (default a.out)");
        eprintln!("  -S          print the generated assembly instead");
        eprintln!("  -E          print the program after macro expansion instead");
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
    if args.iter().any(|a| a == "-E") {
        print!("{}", quill::expand_source(&src, input).unwrap_or_else(|e| fail(&e)));
        return;
    }
    let asm = quill::compile_source(&src, input).unwrap_or_else(|e| fail(&e));

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

fn fail(msg: &str) -> ! {
    eprintln!("{NAME}: {msg}");
    std::process::exit(1);
}

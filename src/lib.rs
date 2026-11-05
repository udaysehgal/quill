//! quill as a library: the whole compiler except assembling and linking,
//! which `main.rs` hands to clang. The browser playground uses this too.
//!
//! Pipeline: source text --(reader)--> Expr --(expander)--> core forms
//!           --(compiler)--> ARM64 assembly

pub mod compiler;
pub mod expand;
pub mod reader;
pub mod syntax;

/// Compile Scheme source to ARM64 assembly. Errors are `file:line: message`.
pub fn compile_source(src: &str, file: &str) -> Result<String, String> {
    let forms = reader::read_all(src).map_err(|e| locate(file, &e))?;
    compiler::compile_program(&forms, file).map_err(|e| locate(file, &e))
}

/// The program after macro expansion, as Scheme source in core forms.
pub fn expand_source(src: &str, file: &str) -> Result<String, String> {
    let forms = reader::read_all(src).map_err(|e| locate(file, &e))?;
    let program = compiler::expand_program(&forms).map_err(|e| locate(file, &e))?;
    Ok(program.to_source(78))
}

/// `line 3: msg` -> `file.scm:3: msg`
pub fn locate(file: &str, err: &str) -> String {
    if let Some(rest) = err.strip_prefix("line ")
        && let Some((n, msg)) = rest.split_once(": ")
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return format!("{file}:{n}: {msg}");
    }
    format!("{file}: {err}")
}

//! Build-script support shared by the workspace crates that embed a directory
//! of TOML files into their binary.
//!
//! Build scripts cannot import each other, so the one embedding routine lives
//! here and every caller gets the same name rule, ordering and failure modes.

use std::path::{Path, PathBuf};

/// An optional `&[&str]` constant listing the keys of the pair constant in
/// the same order, generated from the same directory scan.
#[derive(Debug, Clone, Copy)]
pub struct KeysConst<'a> {
    /// The constant's identifier.
    pub name: &'a str,
    /// The doc comment, one element per line, without the `///` prefix.
    pub doc: &'a [&'a str],
}

/// One embedding: which directory to read and what to write into `OUT_DIR`.
#[derive(Debug, Clone, Copy)]
pub struct EmbedTomlDir<'a> {
    /// The source directory, relative to the calling crate's manifest.
    pub source_dir: &'a str,
    /// The file name written under `OUT_DIR`, for the caller to `include!`.
    pub out_file: &'a str,
    /// The identifier of the generated `&[(&str, &str)]` constant.
    pub const_name: &'a str,
    /// The doc comment of that constant, one element per line, without the
    /// `///` prefix.
    pub doc: &'a [&'a str],
    /// An optional parallel constant holding only the keys.
    pub keys_const: Option<KeysConst<'a>>,
}

/// Embeds every `*.toml` file directly under `spec.source_dir` as one
/// `(stem, include_str!(path))` pair of a `pub(crate)` constant, written to
/// `OUT_DIR/spec.out_file`, and asks cargo to rerun when any of them changes.
///
/// Pairs are in byte order of their stem, so the generated list does not
/// depend on the order the filesystem hands entries back in. Every file stem
/// must satisfy [`yagura_protocol::is_valid_token`]: a definition name and a
/// profile id obey the same rule.
///
/// # Errors
/// Fails when `CARGO_MANIFEST_DIR` or `OUT_DIR` is unset, the directory cannot
/// be read, a file name is not UTF-8 or its stem is not a valid token, the
/// directory holds no `*.toml` file, or the output cannot be written.
pub fn embed_toml_dir(spec: &EmbedTomlDir<'_>) -> Result<(), String> {
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let source_dir = manifest_dir.join(spec.source_dir);
    println!("cargo:rerun-if-changed={}", source_dir.display());

    let mut files = collect_toml_files(&source_dir)?;
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "no *.toml file found under {}",
            source_dir.display()
        ));
    }

    for (_, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let generated = generate(spec, &files);

    let out_path = PathBuf::from(env_var("OUT_DIR")?).join(spec.out_file);
    std::fs::write(&out_path, generated)
        .map_err(|error| format!("failed to write {}: {error}", out_path.display()))
}

/// The source written to `OUT_DIR`: the pair constant, then the keys
/// constant when the spec asks for one, both in the order of `files`.
fn generate(spec: &EmbedTomlDir<'_>, files: &[(String, PathBuf)]) -> String {
    let mut generated = doc_lines(spec.doc);
    generated.push_str(&format!(
        "pub(crate) const {}: &[(&str, &str)] = &[\n",
        spec.const_name
    ));
    for (key, path) in files {
        generated.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            key,
            path.display().to_string()
        ));
    }
    generated.push_str("];\n");

    if let Some(keys_const) = spec.keys_const {
        generated.push('\n');
        generated.push_str(&doc_lines(keys_const.doc));
        generated.push_str(&format!(
            "pub(crate) const {}: &[&str] = &[\n",
            keys_const.name
        ));
        for (key, _) in files {
            generated.push_str(&format!("    {key:?},\n"));
        }
        generated.push_str("];\n");
    }
    generated
}

fn doc_lines(lines: &[&str]) -> String {
    lines.iter().map(|line| format!("/// {line}\n")).collect()
}

/// The `(stem, absolute path)` pair of every `*.toml` file directly under
/// `source_dir`. Subdirectories are ignored: one entry is one file.
fn collect_toml_files(source_dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let read_failed =
        |error: std::io::Error| format!("failed to read {}: {error}", source_dir.display());
    let mut files = Vec::new();
    for entry in std::fs::read_dir(source_dir).map_err(read_failed)? {
        let path = entry.map_err(read_failed)?.path();
        if !path.is_file() || path.extension().is_none_or(|extension| extension != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Err(format!("file name is not valid UTF-8: {}", path.display()));
        };
        if !yagura_protocol::is_valid_token(stem) {
            return Err(format!(
                "file stem {stem:?} of {} does not satisfy [a-z0-9_-]{{1,64}}",
                path.display()
            ));
        }
        files.push((stem.to_owned(), path.clone()));
    }
    Ok(files)
}

/// One build-time environment variable cargo is expected to set.
fn env_var(name: &str) -> Result<std::ffi::OsString, String> {
    std::env::var_os(name).ok_or_else(|| format!("{name} is not set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the temp root, removed on drop.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "yagura-build-support-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }

        fn write(&self, name: &str) {
            std::fs::write(self.0.join(name), "").expect("write scratch file");
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec<'a>(source_dir: &'a str, keys_const: Option<KeysConst<'a>>) -> EmbedTomlDir<'a> {
        EmbedTomlDir {
            source_dir,
            out_file: "embedded.rs",
            const_name: "PAIRS",
            doc: &["Pairs.", "Second line."],
            keys_const,
        }
    }

    #[test]
    fn only_toml_files_directly_under_the_directory_are_collected() {
        let dir = ScratchDir::new("collect");
        dir.write("beta.toml");
        dir.write("alpha.toml");
        dir.write("notes.txt");
        dir.write("README");
        std::fs::create_dir(dir.0.join("nested.toml"))
            .expect("create a directory named like a file");

        let mut files = collect_toml_files(&dir.0).expect("collect the toml files");
        files.sort();

        assert_eq!(
            files,
            vec![
                ("alpha".to_owned(), dir.0.join("alpha.toml")),
                ("beta".to_owned(), dir.0.join("beta.toml")),
            ]
        );
    }

    #[test]
    fn a_stem_that_is_not_a_valid_token_is_rejected() {
        let dir = ScratchDir::new("bad-stem");
        dir.write("Bad.toml");

        let error = collect_toml_files(&dir.0).expect_err("an upper-case stem must be rejected");

        assert!(
            error.contains("\"Bad\""),
            "the error must name the stem: {error}"
        );
    }

    #[test]
    fn a_directory_without_toml_files_is_an_error() {
        let dir = ScratchDir::new("empty");
        dir.write("notes.txt");
        let source_dir = dir.0.to_str().expect("utf8 scratch path");

        let error = embed_toml_dir(&spec(source_dir, None))
            .expect_err("embedding nothing must fail the build");

        assert!(
            error.contains("no *.toml file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_unset_build_variable_is_an_error() {
        let error = env_var("YAGURA_BUILD_SUPPORT_NEVER_SET")
            .expect_err("an unset variable must be an error");

        assert_eq!(error, "YAGURA_BUILD_SUPPORT_NEVER_SET is not set");
    }

    #[test]
    fn the_generated_source_holds_both_constants_in_the_order_given() {
        let files = [
            ("alpha".to_owned(), PathBuf::from("/defs/alpha.toml")),
            ("beta".to_owned(), PathBuf::from("/defs/beta.toml")),
        ];
        let keys = KeysConst {
            name: "KEYS",
            doc: &["Keys."],
        };

        let generated = generate(&spec("defs", Some(keys)), &files);

        assert_eq!(
            generated,
            "/// Pairs.\n\
             /// Second line.\n\
             pub(crate) const PAIRS: &[(&str, &str)] = &[\n    \
             (\"alpha\", include_str!(\"/defs/alpha.toml\")),\n    \
             (\"beta\", include_str!(\"/defs/beta.toml\")),\n\
             ];\n\
             \n\
             /// Keys.\n\
             pub(crate) const KEYS: &[&str] = &[\n    \
             \"alpha\",\n    \
             \"beta\",\n\
             ];\n"
        );
    }
}

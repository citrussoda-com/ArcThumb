//! Build script: compile the Slint UI for `arcthumb-config.exe`,
//! bundle its translations, and embed a Windows resource file
//! containing the app manifest + icon.
//!
//! The manifest declares Per-Monitor DPI v2 and Common Controls v6
//! so `arcthumb-config.exe` scales correctly on mixed-DPI setups and
//! picks up the modern visual style.
//!
//! Cargo links the compiled `.res` into every output artifact, so
//! `arcthumb.dll` also carries the manifest. This is harmless —
//! the shell extension DLL ignores its own manifest.
//!
//! ## Translations
//!
//! Slint's gettext backend only works on Unix (the `gettext-rs`
//! dependency is `cfg(unix)` inside `i-slint-core`), so the GUI uses
//! Slint's bundled translations instead: `slint-build` reads
//! `<dir>/<tag>/LC_MESSAGES/arcthumb.po` catalogs itself, in pure
//! Rust, and compiles them into the binary. No gettext toolchain is
//! needed to build, and a malformed `.po` is a build error rather
//! than a silently untranslated UI.
//!
//! One gettext convention Slint does not follow: an entry with an
//! empty `msgstr` (or a `fuzzy` one) is bundled as an empty string,
//! so a half-translated catalog would blank out the untranslated
//! widgets. To get the usual "untranslated shows the source text"
//! behaviour, the catalogs under `lang/` are first copied to
//! `$OUT_DIR/lang/` with every such entry filled in with its
//! `msgid`, and Slint bundles the copies. Translators therefore only
//! need to fill in what they translate.
//!
//! The list of bundled language tags is also written to
//! `$OUT_DIR/bundled_languages.rs` so `locale.rs` can offer exactly
//! those languages in the dropdown without a hand-maintained list.

use std::fs;
use std::path::{Path, PathBuf};

/// Where the `.po` catalogs live, relative to the crate root. The
/// layout inside is the one `slint-build` expects:
/// `<LANG_DIR>/<tag>/LC_MESSAGES/<crate name>.po`.
const LANG_DIR: &str = "lang";

fn main() {
    // Only rerun when the resources change.
    println!("cargo:rerun-if-changed=resources/arcthumb-config.rc");
    println!("cargo:rerun-if-changed=resources/arcthumb-config.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=ui/main.slint");
    // A directory here means "any file under it", which covers both
    // edited and newly added `.po` files.
    println!("cargo:rerun-if-changed={LANG_DIR}");

    // On non-Windows targets this is a no-op so `cargo check` on
    // other platforms still works. The manifest is mandatory (it
    // declares DPI awareness and Common Controls v6), so treat a
    // missing RC compiler or a failed compile as a hard build error
    // rather than silently shipping a binary without it.
    #[cfg(target_os = "windows")]
    embed_resource::compile("resources/arcthumb-config.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed Windows resource (manifest + icon)");

    // `unrar_sys` compiles RAR's crypt.cpp, which calls CryptGenRandom
    // and friends from advapi32, but its own build script only links
    // shell32. Until the move to sevenz-rust2 the import happened to
    // arrive through a transitive dependency of the old 7z crate;
    // declare it here so linking does not depend on what the rest of
    // the tree pulls in.
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-link-lib=advapi32");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let crate_name = std::env::var("CARGO_PKG_NAME").expect("CARGO_PKG_NAME is set by cargo");

    let bundle_dir = out_dir.join(LANG_DIR);
    let tags = prepare_catalogs(&crate_name, &bundle_dir);
    write_bundled_language_list(&out_dir, &tags);

    // Compile the Slint UI for arcthumb-config. Generated Rust code
    // lands in OUT_DIR and is pulled in by `slint::include_modules!()`.
    //
    // No default translation context: the `.po` files carry plain
    // `msgid`s with no `msgctxt`, which keeps them readable for
    // translators who never see the `.slint` source. Keep this in
    // sync with the `--no-default-translation-context` flag used when
    // regenerating `lang/arcthumb.pot` (see README.md).
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations(bundle_dir)
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/main.slint", config).expect("failed to compile Slint UI");
}

/// Copy every `lang/<tag>/LC_MESSAGES/<crate>.po` into `bundle_dir`
/// with untranslated entries filled in with their `msgid`, and return
/// the tags found, sorted.
///
/// Entries with a `msgid_plural` are left alone: the GUI has none,
/// and the right fallback for a partially filled plural set is not
/// obvious, so a translator who adds one has to complete it.
fn prepare_catalogs(crate_name: &str, bundle_dir: &Path) -> Vec<String> {
    let catalog_name = format!("{crate_name}.po");
    let mut tags = Vec::new();
    for entry in fs::read_dir(LANG_DIR).expect("lang/ directory is missing") {
        let entry = entry.expect("failed to read lang/ entry");
        let source = entry.path().join("LC_MESSAGES").join(&catalog_name);
        if !source.is_file() {
            continue;
        }
        let tag = entry.file_name().to_string_lossy().into_owned();
        assert!(
            tag != "en",
            "lang/en/ must not exist: English is the source language and needs no catalog"
        );

        let mut catalog = rspolib::pofile(source.as_path())
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", source.display()));
        for po_entry in &mut catalog.entries {
            let translated = po_entry.msgstr.as_deref().is_some_and(|s| !s.is_empty());
            if po_entry.msgid_plural.is_none() && (!translated || po_entry.fuzzy()) {
                po_entry.msgstr = Some(po_entry.msgid.clone());
                po_entry.flags.retain(|flag| flag != "fuzzy");
            }
        }

        let target_dir = bundle_dir.join(&tag).join("LC_MESSAGES");
        fs::create_dir_all(&target_dir).expect("failed to create the translation bundle dir");
        fs::write(target_dir.join(&catalog_name), catalog.to_string())
            .expect("failed to write the prepared catalog");
        tags.push(tag);
    }
    tags.sort();
    tags
}

/// Emit `pub const LANGUAGES: &[&str]` for `locale.rs`.
///
/// English comes first because it is the source language of the
/// `.slint` file and has no `.po` of its own; the rest follow in
/// sorted order so the dropdown is stable across builds.
fn write_bundled_language_list(out_dir: &Path, tags: &[String]) {
    let mut source = String::from(
        "// Generated by build.rs from the `.po` files under lang/.\n\
         // English first (source language), then the bundled catalogs.\n\
         pub const LANGUAGES: &[&str] = &[\"en\"",
    );
    for tag in tags {
        source.push_str(&format!(", {tag:?}"));
    }
    source.push_str("];\n");
    fs::write(out_dir.join("bundled_languages.rs"), source)
        .expect("failed to write bundled_languages.rs");
}

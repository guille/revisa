use std::path::{Path, PathBuf};
use syntect::parsing::SyntaxSet;

/// Bundled syntax set, checked into the repo at assets/bundled_syntaxes.bin.
static BUNDLED_SYNTAXES: &[u8] = include_bytes!("../../assets/bundled_syntaxes.bin");

/// A simple hash of the bundled blob, used for cache invalidation.
/// When the bundled set changes, user caches built from a different base are stale.
fn bundled_hash() -> u64 {
    // FNV-1a hash — no extra dependencies needed.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in BUNDLED_SYNTAXES {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Return the default directory for user-provided `.sublime-syntax` files.
/// `$XDG_CONFIG_HOME/revisa/syntaxes/` (defaults to `~/.config/revisa/syntaxes/`).
pub fn default_syntaxes_dir() -> PathBuf {
    config_dir().join("syntaxes")
}

/// Return the path where the user's compiled syntax cache is stored.
/// `$XDG_CACHE_HOME/revisa/syntaxes.bin` (defaults to `~/.cache/revisa/syntaxes.bin`).
pub fn syntax_cache_path() -> PathBuf {
    cache_dir().join("syntaxes.bin")
}

/// Sidecar file storing the hash of the bundled blob the cache was built from.
fn cache_meta_path() -> PathBuf {
    cache_dir().join("syntaxes.meta")
}

/// Load the bundled (embedded) syntax set.
pub fn load_bundled_syntaxes() -> SyntaxSet {
    syntect::dumps::from_binary(BUNDLED_SYNTAXES)
}

/// Build the user's syntax cache from the embedded bundle + extra `.sublime-syntax` files.
///
/// Extras override bundled syntaxes when they match the same file extensions/scopes.
/// The compiled cache is written to [`syntax_cache_path()`].
///
/// Returns the number of syntaxes in the compiled set.
pub fn build_syntax_cache(syntaxes_dir: &Path) -> Result<usize, String> {
    let mut builder = load_bundled_syntaxes().into_builder();

    if syntaxes_dir.is_dir() {
        builder.add_from_folder(syntaxes_dir, true).map_err(|e| {
            format!(
                "failed to load syntaxes from {}: {e}",
                syntaxes_dir.display()
            )
        })?;
    } else {
        eprintln!(
            "Note: syntaxes directory {} does not exist, building from bundled syntaxes only.",
            syntaxes_dir.display()
        );
    }

    let syntax_set = builder.build();
    let count = syntax_set.syntaxes().len();

    let cache_path = syntax_cache_path();
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create cache directory: {e}"))?;
    }

    syntect::dumps::dump_to_file(&syntax_set, &cache_path)
        .map_err(|e| format!("failed to write cache to {}: {e}", cache_path.display()))?;

    // Write sidecar with the hash of the bundled blob this cache was built from.
    let _ = std::fs::write(cache_meta_path(), bundled_hash().to_string());

    Ok(count)
}

/// Build the bundled syntax cache from source `.sublime-syntax` files.
/// Writes to `assets/bundled_syntaxes.bin` in the project root.
///
/// This is a maintainer-only command, not shipped in release builds.
#[cfg(feature = "dev-tools")]
pub fn build_bundled_cache(syntaxes_dir: &Path) -> Result<usize, String> {
    use syntect::parsing::SyntaxSetBuilder;

    let mut builder = SyntaxSetBuilder::new();
    builder.add_plain_text_syntax();

    if syntaxes_dir.is_dir() {
        builder.add_from_folder(syntaxes_dir, true).map_err(|e| {
            format!(
                "failed to load syntaxes from {}: {e}",
                syntaxes_dir.display()
            )
        })?;
    } else {
        return Err(format!(
            "syntaxes directory {} does not exist",
            syntaxes_dir.display()
        ));
    }

    let syntax_set = builder.build();
    let count = syntax_set.syntaxes().len();

    let out_path = PathBuf::from("assets/bundled_syntaxes.bin");
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create assets directory: {e}"))?;
    }

    syntect::dumps::dump_to_file(&syntax_set, &out_path)
        .map_err(|e| format!("failed to write bundled cache: {e}"))?;

    Ok(count)
}

/// Try to load the user's compiled syntax cache. Returns `None` if absent, corrupt,
/// or built from a different bundled base (stale).
pub fn load_syntax_cache() -> Option<SyntaxSet> {
    let path = syntax_cache_path();
    if !path.exists() {
        return None;
    }

    // Check if the cache was built from the current bundled base.
    if let Ok(meta) = std::fs::read_to_string(cache_meta_path())
        && let Ok(stored_hash) = meta.trim().parse::<u64>()
        && stored_hash != bundled_hash()
    {
        eprintln!("Note: syntax cache was built from an older bundled base, ignoring.");
        eprintln!("Hint: re-run `revisa build-cache` to rebuild with the latest base.");
        return None;
    }

    match syntect::dumps::from_dump_file(&path) {
        Ok(ss) => Some(ss),
        Err(e) => {
            eprintln!("Warning: failed to load revisa syntax cache: {e}");
            eprintln!("Hint: re-run `revisa build-cache` to rebuild.");
            None
        }
    }
}

fn config_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map_or_else(|_| home_dir().join(".config"), PathBuf::from)
        .join(env!("CARGO_PKG_NAME"))
}

fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map_or_else(|_| home_dir().join(".cache"), PathBuf::from)
        .join(env!("CARGO_PKG_NAME"))
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

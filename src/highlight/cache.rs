use std::path::{Path, PathBuf};
use syntect::parsing::SyntaxSet;

/// Whether to attempt loading bat's syntax cache. Set to `false` until bat
/// ships with a syntect version whose binary dumps are compatible with ours.
pub const BAT_SYNTAXES_ENABLED: bool = false;

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

/// Try to load bat's compiled syntax cache.
/// Returns `None` if bat's cache is missing, corrupt, or loading is disabled.
fn load_bat_syntaxes() -> Option<SyntaxSet> {
    if !BAT_SYNTAXES_ENABLED {
        return None;
    }

    let bat_cache_path = bat_cache_dir().join("syntaxes.bin");
    if !bat_cache_path.exists() {
        return None;
    }

    match syntect::dumps::from_dump_file(&bat_cache_path) {
        Ok(ss) => Some(ss),
        Err(e) => {
            eprintln!("Note: failed to load bat syntax cache: {e}");
            None
        }
    }
}

/// Locate bat's cache directory.
/// Respects `BAT_CACHE_PATH`, then `$XDG_CACHE_HOME/bat`, then `~/.cache/bat`.
fn bat_cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("BAT_CACHE_PATH") {
        return PathBuf::from(p);
    }
    let base =
        std::env::var("XDG_CACHE_HOME").map_or_else(|_| home_dir().join(".cache"), PathBuf::from);
    base.join("bat")
}

/// Build the user's syntax cache from bat (if available) + bundled + user extras.
///
/// Load order (last-added wins in syntect's `.rev()` lookup):
/// 1. Bundled syntaxes (base)
/// 2. Bat's syntax cache (if present and enabled — wider language coverage)
/// 3. User's `.sublime-syntax` files from `syntaxes_dir` (highest priority)
///
/// The compiled cache is written to [`syntax_cache_path()`], unless it would add
/// nothing to the bundled set.
///
/// Returns how many syntaxes the cache adds beyond the bundled set; zero means
/// nothing was written.
pub fn build_syntax_cache(syntaxes_dir: &Path) -> Result<usize, String> {
    let bundled = load_bundled_syntaxes();
    let base_count = bundled.syntaxes().len();
    let mut builder = bundled.into_builder();

    // Layer bat's syntaxes on top of bundled (if available).
    if let Some(bat_ss) = load_bat_syntaxes() {
        let bat_count = bat_ss.syntaxes().len();
        for syntax in bat_ss.into_builder().syntaxes().to_vec() {
            builder.add(syntax);
        }
        eprintln!("Loaded {bat_count} syntaxes from bat's cache.");
    }

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
    let total = syntax_set.syntaxes().len();
    let extras = total.saturating_sub(base_count);

    if extras == 0 {
        return Ok(0);
    }

    let cache_path = syntax_cache_path();
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create cache directory: {e}"))?;
    }

    syntect::dumps::dump_to_file(&syntax_set, &cache_path)
        .map_err(|e| format!("failed to write cache to {}: {e}", cache_path.display()))?;

    // Write sidecar with the hash of the bundled blob this cache was built from.
    let _ = std::fs::write(cache_meta_path(), bundled_hash().to_string());

    Ok(extras)
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
        eprintln!("Note: ignoring syntax cache built from an older bundled syntax set.");
        eprintln!("Hint: re-run `revisa build-cache` to pick up your custom syntaxes again.");
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

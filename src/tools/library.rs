//! `list_separations`: the SplitFire app's own separation history, read from disk.
//!
//! The app stores each song as a folder of WAV stems plus a `splitfire.json` sidecar, under
//! either its iCloud Drive Documents container or the pre-iCloud app-data `separated/` tree.
//! It rebuilds its history by scanning exactly that layout, so this tool is read-only by
//! design: writing new stems belongs to `separate_stems` and deleting belongs to the app.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolCtx, ToolOutcome, function_def};

pub const NAME: &str = "list_separations";

/// The app's sidecar, as written by `stems_storage::SeparationSidecar`.
#[derive(Deserialize)]
struct Sidecar {
    model_id: String,
    /// Unix millis at separation time on the originating device.
    separated_at_ms: u64,
    /// Original input file name, with extension.
    source_file: String,
    /// Stem ids written, e.g. "vocals", "drums".
    stems: Vec<String>,
}

/// One discovered song folder.
struct Separation {
    dir: PathBuf,
    /// Which root it came from, in the app's vocabulary.
    backend: &'static str,
    sidecar: Sidecar,
    /// Stem files actually present, paired with their size in bytes.
    files: Vec<(PathBuf, u64)>,
}

pub fn definition() -> Value {
    function_def(
        NAME,
        "List the songs the SplitFire app has already separated, with the model used, when it ran, \
         and the stem files on disk. Read-only: it reports the app's existing library and never \
         changes it. Use it to answer questions about past separations or to find a stem to analyze.",
        json!({
            "type": "object",
            "properties": {
                "song": {
                    "type": "string",
                    "description": "Only report songs whose folder or source file name contains this text (case-insensitive)"
                }
            }
        }),
    )
}

#[derive(Deserialize)]
struct Args {
    song: Option<String>,
}

pub async fn execute(ctx: &ToolCtx, args: Value) -> Result<ToolOutcome> {
    let a: Args = serde_json::from_value(args)?;
    let roots = stem_roots();
    let filter = a.song.as_deref().map(str::to_lowercase);

    let found = {
        let roots = roots.clone();
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || collect(&roots, filter.as_deref()))
    };
    let mut found = tokio::select! {
        r = found => r?,
        () = ctx.cancel.cancelled() => return Ok(ToolOutcome::err("Listing cancelled.")),
    };
    // Newest first; the app presents history the same way.
    found.sort_by_key(|f| std::cmp::Reverse(f.sidecar.separated_at_ms));

    let mut out = ToolOutcome::ok(report(&roots, &found, filter.as_deref()));
    out.locations = found.iter().map(|s| s.dir.clone()).collect();
    Ok(out)
}

/// Where the app keeps stems, most-specific first. `SPLITFIRE_STEMS_DIR` replaces the
/// defaults outright: discovery is platform- and sandbox-specific, so it is the escape hatch
/// when the real roots live somewhere this code would not guess.
fn stem_roots() -> Vec<(PathBuf, &'static str)> {
    if let Some(raw) = std::env::var_os("SPLITFIRE_STEMS_DIR")
        && !raw.is_empty()
    {
        return std::env::split_paths(&raw)
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| (p, "configured"))
            .collect();
    }
    let Some(dirs) = directories::BaseDirs::new() else {
        return Vec::new();
    };
    let home = dirs.home_dir();
    let mut roots = Vec::new();
    // Apple platforms: the app's iCloud Drive Documents container.
    if cfg!(target_os = "macos") {
        roots.push((
            home.join("Library/Mobile Documents/iCloud~ai~splitfire~SplitfireAI/Documents"),
            "icloud",
        ));
    }
    // Everywhere else, and for pre-iCloud separations: `<app data>/separated`.
    // Both the release and the debug bundle identifier, since either may have run here.
    for id in ["ai.splitfire.SplitfireAI", "ai.splitfire.SplitfireAI-Debug"] {
        roots.push((dirs.data_dir().join(id).join("separated"), "local"));
    }
    roots
}

/// Scan each root's immediate children for a sidecar. Best-effort throughout: a missing or
/// unreadable root, or a folder without a readable sidecar, is simply not reported.
fn collect(roots: &[(PathBuf, &'static str)], filter: Option<&str>) -> Vec<Separation> {
    let mut out = Vec::new();
    for (root, backend) in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(bytes) = std::fs::read(dir.join("splitfire.json")) else {
                continue;
            };
            let Ok(sidecar) = serde_json::from_slice::<Sidecar>(&bytes) else {
                continue;
            };
            if let Some(needle) = filter {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !name.contains(needle) && !sidecar.source_file.to_lowercase().contains(needle) {
                    continue;
                }
            }
            let files = stem_files(&dir, &sidecar);
            out.push(Separation {
                dir,
                backend,
                sidecar,
                files,
            });
        }
    }
    out
}

/// The stem files for a song, in the sidecar's order. A stem listed but absent is reported as
/// missing rather than silently dropped: on iCloud it may simply not be downloaded yet.
fn stem_files(dir: &Path, sidecar: &Sidecar) -> Vec<(PathBuf, u64)> {
    sidecar
        .stems
        .iter()
        .map(|stem| {
            let path = dir.join(format!("{stem}.wav"));
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            (path, size)
        })
        .collect()
}

fn report(roots: &[(PathBuf, &'static str)], found: &[Separation], filter: Option<&str>) -> String {
    if found.is_empty() {
        let mut s = match filter {
            Some(f) => format!("No separated songs matching `{f}`."),
            None => "No separated songs found.".to_string(),
        };
        s.push_str("\n\nSearched:");
        for (root, _) in roots {
            s.push_str(&format!("\n- {}", root.display()));
        }
        if roots.is_empty() {
            s.push_str("\n- (no stem roots could be resolved)");
        }
        s.push_str(
            "\n\nIf the app stores stems somewhere else, set SPLITFIRE_STEMS_DIR to that directory.",
        );
        return s;
    }

    let plural = if found.len() == 1 { "song" } else { "songs" };
    let mut s = format!("{} separated {plural}:\n", found.len());
    for sep in found {
        let name = sep
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| sep.dir.display().to_string());
        s.push_str(&format!(
            "\n{name}\n  model: {}\n  separated: {} ({})\n  source: {}\n  folder: {}\n",
            sep.sidecar.model_id,
            iso8601_ms(sep.sidecar.separated_at_ms),
            sep.backend,
            sep.sidecar.source_file,
            sep.dir.display(),
        ));
        for (path, size) in &sep.files {
            let stem = path
                .file_stem()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if *size == 0 {
                s.push_str(&format!(
                    "  - {stem}: not on this device ({})\n",
                    path.display()
                ));
            } else {
                s.push_str(&format!(
                    "  - {stem}: {} ({})\n",
                    human_bytes(*size),
                    path.display()
                ));
            }
        }
    }
    s
}

fn human_bytes(n: u64) -> String {
    const MIB: f64 = (1 << 20) as f64;
    if n >= (1 << 20) {
        format!("{:.1} MiB", n as f64 / MIB)
    } else {
        format!("{:.0} KiB", (n as f64 / 1024.0).max(1.0))
    }
}

/// Unix millis as a UTC timestamp, via the same civil-date maths as `crate::agent::iso8601`.
fn iso8601_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = era * 400 + yoe + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_song(root: &Path, name: &str, sidecar: &str, stems: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("splitfire.json"), sidecar).unwrap();
        for stem in stems {
            std::fs::write(dir.join(format!("{stem}.wav")), vec![0u8; 2048]).unwrap();
        }
    }

    /// The real sidecar the app writes, copied from a separation on disk.
    const REAL_SIDECAR: &str = r#"{
      "model_id": "htdemucs",
      "separated_at_ms": 1783395299798,
      "source_file": "splitfire-test-song.wav",
      "stems": ["drums", "bass", "other", "vocals"]
    }"#;

    #[test]
    fn reads_a_real_shaped_sidecar_tree() {
        let tmp = std::env::temp_dir().join(format!("sf-lib-{}", uuid::Uuid::new_v4()));
        let root = tmp.join("Documents");
        write_song(
            &root,
            "splitfire-test-song",
            REAL_SIDECAR,
            &["drums", "bass", "other", "vocals"],
        );
        // A folder with no sidecar, and one with a corrupt sidecar: both skipped, not fatal.
        std::fs::create_dir_all(root.join("not-a-separation")).unwrap();
        write_song(&root, "corrupt", "{ not json", &[]);

        let roots = vec![(root.clone(), "icloud")];
        let found = collect(&roots, None);
        assert_eq!(found.len(), 1, "only the valid sidecar is reported");
        let sep = &found[0];
        assert_eq!(sep.sidecar.model_id, "htdemucs");
        assert_eq!(sep.sidecar.source_file, "splitfire-test-song.wav");
        assert_eq!(sep.sidecar.stems.len(), 4);
        assert_eq!(sep.files.len(), 4);
        assert!(sep.files.iter().all(|(_, size)| *size == 2048));

        let text = report(&roots, &found, None);
        assert!(text.contains("1 separated song"), "{text}");
        assert!(text.contains("htdemucs"), "{text}");
        assert!(text.contains("2026-"), "timestamp rendered: {text}");
        assert!(text.contains("vocals"), "{text}");

        // A filter that matches the source file but not the folder name still hits.
        assert_eq!(collect(&roots, Some("test-song")).len(), 1);
        assert_eq!(collect(&roots, Some("nothing")).len(), 0);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_roots_are_not_an_error() {
        let roots = vec![(PathBuf::from("/nonexistent/splitfire/stems"), "local")];
        let found = collect(&roots, None);
        assert!(found.is_empty());
        let text = report(&roots, &found, None);
        assert!(text.contains("No separated songs found"), "{text}");
        assert!(text.contains("SPLITFIRE_STEMS_DIR"), "{text}");
    }

    #[test]
    fn a_stem_absent_from_disk_is_reported_as_not_downloaded() {
        let tmp = std::env::temp_dir().join(format!("sf-lib-{}", uuid::Uuid::new_v4()));
        let root = tmp.join("Documents");
        // Sidecar lists four stems; only two were downloaded from iCloud.
        write_song(&root, "half-synced", REAL_SIDECAR, &["drums", "bass"]);
        let roots = vec![(root, "icloud")];
        let found = collect(&roots, None);
        let text = report(&roots, &found, None);
        assert!(text.contains("vocals: not on this device"), "{text}");
        assert!(text.contains("drums: 2 KiB"), "{text}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn stems_dir_env_var_replaces_the_defaults() {
        // Safety: single-threaded assertion on process env, restored before returning.
        let key = "SPLITFIRE_STEMS_DIR";
        let prev = std::env::var_os(key);
        unsafe { std::env::set_var(key, "/tmp/sf-one") };
        let roots = stem_roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].0, PathBuf::from("/tmp/sf-one"));
        assert_eq!(roots[0].1, "configured");
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn epoch_millis_render_as_utc() {
        assert_eq!(iso8601_ms(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_ms(1_783_395_299_798), "2026-07-07T03:34:59Z");
    }
}

//! Command line interface: argument definitions and the value parsers
//! used by clap for `--min-size` and `--older-than`.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use crate::models::Ecosystem;

/// Dev Cache & Artifact Cleaner — find and safely remove build artifacts and caches.
// The bools are exactly the CLI flag set; enums would not buy anything here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Parser)]
#[command(
    name = "devclean",
    version,
    about = "Dev Cache & Artifact Cleaner — find and safely remove build artifacts and caches",
    after_help = "Examples:
  devclean                                  full-screen TUI: pick folder, scan, clean
  devclean ~/Projects --older-than 30d
  devclean /data/dev --interactive --type node,rust
  devclean ~/Projects --tui
  devclean --menu                           old step-by-step menu
  devclean ~/Projects --all --yes"
)]
pub struct Cli {
    /// Directory to scan. Defaults to the current directory.
    pub dir: Option<PathBuf>,

    /// Pick artifacts for removal with an interactive checklist.
    #[arg(short, long, conflicts_with = "all")]
    pub interactive: bool,

    /// Pick artifacts in a full-screen TUI (ratatui) and delete the ticked ones.
    #[arg(long, conflicts_with_all = ["interactive", "all", "dry_run"])]
    pub tui: bool,

    /// Open the step-by-step dialoguer menu instead of the full-screen TUI.
    #[arg(long, conflicts_with_all = ["interactive", "all", "tui", "dry_run"])]
    pub menu: bool,

    /// Mark every found artifact for removal. Requires --yes to run.
    #[arg(long)]
    pub all: bool,

    /// Skip the prompt: go ahead with --all without asking.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Show what would be removed without touching the filesystem.
    #[arg(long)]
    pub dry_run: bool,

    /// Only artifacts unchanged for this long, e.g. 14d, 2w, 1m (= a month).
    #[arg(long, value_parser = parse_duration)]
    pub older_than: Option<Duration>,

    /// Restrict the scan to ecosystems: node,rust,python,java,dotnet,generic.
    #[arg(long = "type", value_delimiter = ',', value_parser = clap::value_parser!(Ecosystem))]
    pub types: Vec<Ecosystem>,

    /// Ignore artifacts smaller than this, e.g. 100MB, 1.5GB, 256KiB.
    #[arg(long, value_parser = parse_size)]
    pub min_size: Option<u64>,

    /// Path patterns to ignore (repeatable). `*` stays inside one path
    /// segment, `**` crosses segments; a plain pattern is a substring match.
    #[arg(long = "exclude")]
    pub exclude: Vec<String>,
}

impl Cli {
    /// The scan root: the positional arg, or the current directory when omitted.
    #[must_use]
    pub fn start_dir(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Size parser for `--min-size`: `100`, `100KB`, `1.5GB`, `256KiB` (decimal
/// KB/MB/GB and binary KiB/MiB/GiB/TiB are both accepted, case-insensitive).
///
/// # Errors
/// Returns a message describing the number or unit problem.
pub fn parse_size(input: &str) -> Result<u64, String> {
    let lowered: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect::<String>()
        .to_ascii_lowercase();
    let split = lowered
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(lowered.len());
    let (digits, unit) = lowered.split_at(split);
    let value: f64 = digits
        .parse()
        .map_err(|_| format!("bad number in size '{input}'"))?;
    let mult = match unit {
        "" | "b" => 1.0,
        "k" | "kb" => 1_000.0,
        "ki" | "kib" => 1_024.0,
        "m" | "mb" => 1_000_000.0,
        "mib" => 1_048_576.0,
        "g" | "gb" => 1_000_000_000.0,
        "gib" => 1_073_741_824.0,
        "t" | "tb" => 1_000_000_000_000.0,
        "tib" => 1_099_511_627_776.0,
        _ => return Err(format!("unknown size unit '{unit}' in '{input}'")),
    };
    let bytes = value * mult;
    if !(bytes.is_finite() && (0.0..18_446_744_073_709_552.0).contains(&bytes)) {
        return Err(format!("size '{input}' is out of range"));
    }
    // Bounded above by the range check, so truncation/cast is safe here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = bytes.round() as u64;
    Ok(size)
}

/// Duration parser for `--older-than`. Units: `s`, `min`, `h`, `d`, `w`,
/// `m`/`mo` (a month = 30 days) and `y` (365 days). Values may be chained,
/// e.g. `1y2w3d`. `m` means months (the `--older-than 1m, 6m` case of the
/// spec), spelled-out `min` means minutes.
///
/// # Errors
/// Returns a message describing the malformed component or overflow.
pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let lowered: String = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if lowered.is_empty() {
        return Err("empty duration".to_string());
    }
    let mut rest = lowered.as_str();
    let mut total_secs: u64 = 0;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits == 0 {
            return Err(format!("expected a number at '{rest}' in '{input}'"));
        }
        let count: u64 = rest[..digits]
            .parse()
            .map_err(|_| format!("bad number at '{rest}' in '{input}'"))?;
        rest = &rest[digits..];
        let unit_len = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        rest = &rest[unit_len..];
        let secs = match unit {
            "m" | "mo" | "month" | "months" => 30 * 86_400,
            "w" | "week" | "weeks" => 7 * 86_400,
            "d" | "day" | "days" => 86_400,
            "h" | "hour" | "hours" => 3_600,
            "min" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "y" | "year" | "years" => 365 * 86_400,
            "" => return Err(format!("missing unit in '{input}'")),
            _ => return Err(format!("unknown unit '{unit}' in '{input}'")),
        };
        let add = count
            .checked_mul(secs)
            .ok_or_else(|| format!("duration '{input}' overflows"))?;
        total_secs = total_secs
            .checked_add(add)
            .ok_or_else(|| format!("duration '{input}' overflows"))?;
    }
    Ok(Duration::from_secs(total_secs))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{parse_duration, parse_size};

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("100"), Ok(100));
        assert_eq!(parse_size("100MB"), Ok(100_000_000));
        assert_eq!(parse_size("1.5GB"), Ok(1_500_000_000));
        assert_eq!(parse_size("100KiB"), Ok(102_400));
        assert_eq!(parse_size("2 kb"), Ok(2_000));
        assert!(parse_size("1,5GB").is_err(), "decimal comma unsupported");
    }

    #[test]
    fn size_errors() {
        assert!(parse_size("12XB").is_err());
        assert!(parse_size("KB100").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("999999TB").is_err());
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("14d"), Ok(Duration::from_secs(14 * 86_400)));
        // Per the spec examples, 1m / 6m mean months, not minutes.
        assert_eq!(
            parse_duration("6m"),
            Ok(Duration::from_secs(6 * 30 * 86_400))
        );
        assert_eq!(parse_duration("2w"), Ok(Duration::from_secs(14 * 86_400)));
        assert_eq!(
            parse_duration("1y2w3d"),
            Ok(Duration::from_secs(
                365 * 86_400 + 2 * 7 * 86_400 + 3 * 86_400
            ))
        );
        assert_eq!(parse_duration("30min"), Ok(Duration::from_secs(1_800)));
        assert_eq!(parse_duration("3h"), Ok(Duration::from_secs(10_800)));
    }

    #[test]
    fn duration_errors() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("12").is_err());
        assert!(parse_duration("12x").is_err());
        assert!(parse_duration("99999999999999999999y").is_err());
    }
}

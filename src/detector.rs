//! Detection heuristics: decide whether a directory really is a build
//! artifact by its name plus confirmation markers found as sibling files
//! or inside the directory itself.

use std::path::Path;

use log::debug;

use crate::models::ArtifactType;

/// One confirmation marker for a candidate directory.
#[derive(Debug, Clone, Copy)]
enum Marker {
    /// A sibling file with this exact name must exist.
    SiblingFile(&'static str),
    /// A sibling file whose name (lowercased) ends with this suffix.
    SiblingSuffix(&'static str),
    /// A sibling file whose name (lowercased) starts with this prefix.
    SiblingPrefix(&'static str),
    /// A path inside the candidate directory must exist (relative to it).
    Inside(&'static str),
}

/// A detection rule: directory name plus any single marker match confirms it.
#[derive(Debug)]
struct Rule {
    name: &'static str,
    kind: ArtifactType,
    markers: &'static [Marker],
}

/// Artifact-name to confirmation-marker table (spec §2.2).
///
/// Names without markers are unambiguous ecosystem caches that are safe to
/// flag on their own names.
const RULES: &[Rule] = &[
    Rule {
        name: "node_modules",
        kind: ArtifactType::NodeModules,
        markers: &[Marker::SiblingFile("package.json")],
    },
    Rule {
        name: "target",
        kind: ArtifactType::RustTarget,
        markers: &[Marker::SiblingFile("Cargo.toml")],
    },
    // Maven also builds into `target/`; the same name needs its own rule.
    Rule {
        name: "target",
        kind: ArtifactType::JavaBuild,
        markers: &[Marker::SiblingFile("pom.xml")],
    },
    Rule {
        name: ".venv",
        kind: ArtifactType::PythonVenv,
        markers: &[
            Marker::SiblingFile("pyproject.toml"),
            Marker::SiblingFile("requirements.txt"),
            Marker::Inside("pyvenv.cfg"),
            Marker::Inside("bin/python"),
            Marker::Inside("Scripts/python.exe"),
        ],
    },
    Rule {
        name: "venv",
        kind: ArtifactType::PythonVenv,
        markers: &[
            Marker::SiblingFile("pyproject.toml"),
            Marker::SiblingFile("requirements.txt"),
            Marker::Inside("pyvenv.cfg"),
            Marker::Inside("bin/python"),
            Marker::Inside("Scripts/python.exe"),
        ],
    },
    Rule {
        name: "env",
        kind: ArtifactType::PythonVenv,
        markers: &[
            Marker::SiblingFile("pyproject.toml"),
            Marker::SiblingFile("requirements.txt"),
            Marker::Inside("pyvenv.cfg"),
            Marker::Inside("bin/python"),
            Marker::Inside("Scripts/python.exe"),
        ],
    },
    Rule {
        name: "__pycache__",
        kind: ArtifactType::PythonCache,
        markers: &[],
    },
    Rule {
        name: ".pytest_cache",
        kind: ArtifactType::PythonCache,
        markers: &[],
    },
    Rule {
        name: ".mypy_cache",
        kind: ArtifactType::PythonCache,
        markers: &[],
    },
    Rule {
        name: "build",
        kind: ArtifactType::JavaBuild,
        markers: &[
            Marker::SiblingFile("build.gradle"),
            Marker::SiblingFile("build.gradle.kts"),
            Marker::SiblingFile("settings.gradle"),
            Marker::SiblingFile("settings.gradle.kts"),
        ],
    },
    Rule {
        name: ".gradle",
        kind: ArtifactType::JavaBuild,
        markers: &[
            Marker::SiblingFile("build.gradle"),
            Marker::SiblingFile("build.gradle.kts"),
            Marker::SiblingFile("settings.gradle"),
            Marker::SiblingFile("settings.gradle.kts"),
        ],
    },
    // Visual Studio C++ also keeps intermediates in `bin`/`obj`. These rules
    // come first so a `.vcxproj` sibling wins over the `.sln`-based .NET rule
    // (a C++ solution has a `.sln` too).
    Rule {
        name: "bin",
        kind: ArtifactType::CppBuild,
        markers: &[Marker::SiblingSuffix(".vcxproj")],
    },
    Rule {
        name: "obj",
        kind: ArtifactType::CppBuild,
        markers: &[Marker::SiblingSuffix(".vcxproj")],
    },
    Rule {
        name: "bin",
        kind: ArtifactType::DotnetBuild,
        markers: &[
            Marker::SiblingSuffix(".csproj"),
            Marker::SiblingSuffix(".fsproj"),
            Marker::SiblingSuffix(".vbproj"),
            Marker::SiblingSuffix(".sln"),
        ],
    },
    Rule {
        name: "obj",
        kind: ArtifactType::DotnetBuild,
        markers: &[
            Marker::SiblingSuffix(".csproj"),
            Marker::SiblingSuffix(".fsproj"),
            Marker::SiblingSuffix(".vbproj"),
            Marker::SiblingSuffix(".sln"),
        ],
    },
    Rule {
        name: ".turbo",
        kind: ArtifactType::TurboCache,
        markers: &[Marker::SiblingFile("turbo.json")],
    },
    Rule {
        name: ".next",
        kind: ArtifactType::NextCache,
        markers: &[
            Marker::SiblingFile("package.json"),
            Marker::SiblingPrefix("next.config."),
        ],
    },
    Rule {
        name: ".nuxt",
        kind: ArtifactType::NuxtCache,
        markers: &[
            Marker::SiblingFile("package.json"),
            Marker::SiblingPrefix("nuxt.config."),
        ],
    },
    // `dist` is a popular personal-folder name; a sibling ecosystem config
    // (decided with the user) is required to treat it as a build output.
    Rule {
        name: "dist",
        kind: ArtifactType::Dist,
        markers: &[
            Marker::SiblingFile("package.json"),
            Marker::SiblingFile("pyproject.toml"),
        ],
    },
];

/// Detects whether the directory `dir_name` at `dir_path` is an artifact.
///
/// `siblings` holds the names seen next to the candidate in its parent
/// listing. Comparison is ASCII-case-insensitive, matching case-insensitive
/// filesystems (Windows/macOS) without misreading on Linux either.
///
/// Several rules may share a name (`target` is Cargo and Maven): the first one
/// whose markers confirm wins, while marker-less rules match on the name alone.
///
/// Filesystem access happens only for `Inside` markers of matched rules.
#[must_use]
pub fn detect<S: AsRef<str>>(
    dir_name: &str,
    dir_path: &Path,
    siblings: &[S],
) -> Option<ArtifactType> {
    let detected = RULES
        .iter()
        .filter(|rule| dir_name.eq_ignore_ascii_case(rule.name))
        .find(|rule| {
            rule.markers.is_empty()
                || rule
                    .markers
                    .iter()
                    .any(|m| marker_matches(m, dir_path, siblings))
        })
        .map(|rule| {
            debug!("artifact detected: '{dir_name}' -> {:?}", rule.kind);
            rule.kind
        });
    if detected.is_none()
        && RULES
            .iter()
            .any(|rule| dir_name.eq_ignore_ascii_case(rule.name))
    {
        debug!("candidate '{dir_name}' ignored: none of its markers exist");
    }
    detected
}

/// Timestamp/cache dirs are the exception: rules with no markers match on
/// name alone, so a missing marker never cancels them.
fn marker_matches<S: AsRef<str>>(marker: &Marker, dir_path: &Path, siblings: &[S]) -> bool {
    match *marker {
        Marker::SiblingFile(file) => siblings
            .iter()
            .any(|s| s.as_ref().eq_ignore_ascii_case(file)),
        Marker::SiblingSuffix(suffix) => siblings
            .iter()
            .any(|s| s.as_ref().to_ascii_lowercase().ends_with(suffix)),
        Marker::SiblingPrefix(prefix) => siblings
            .iter()
            .any(|s| s.as_ref().to_ascii_lowercase().starts_with(prefix)),
        Marker::Inside(rel) => {
            // Access errors (denied, missing) fail the marker without panicking.
            dir_path.join(rel).try_exists().unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RULES, detect};

    /// Builds a real directory with marker files on disk, needed by `Inside` checks.
    fn tmp(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(name)
            .tempdir()
            .expect("temp dir")
    }

    #[test]
    fn no_marked_name_is_ignored() {
        let d = tmp("plain");
        assert_eq!(detect("src", d.path(), &["main.rs"]), None);
        assert_eq!(detect::<&str>("unknown_cache", d.path(), &[]), None);
    }

    #[test]
    fn node_modules_requires_package_json_nearby() {
        let d = tmp("node");
        assert_eq!(detect("node_modules", d.path(), &["index.js"]), None);
        assert_eq!(
            detect("node_modules", d.path(), &["package.json"]),
            Some(super::ArtifactType::NodeModules)
        );
    }

    #[test]
    fn target_requires_cargo_toml_nearby() {
        let d = tmp("rust");
        assert_eq!(detect("target", d.path(), &["src"]), None);
        assert_eq!(
            detect("target", d.path(), &["Cargo.toml", "src"]),
            Some(super::ArtifactType::RustTarget)
        );
        // Casing matches Windows-style filesystems.
        assert_eq!(
            detect("TARGET", d.path(), &["cargo.toml"]),
            Some(super::ArtifactType::RustTarget)
        );
    }

    #[test]
    fn target_is_maven_when_pom_xml_is_the_marker() {
        let d = tmp("maven");
        assert_eq!(
            detect("target", d.path(), &["pom.xml", "src"]),
            Some(super::ArtifactType::JavaBuild),
            "Maven builds into target/ too"
        );
        // Both markers present: the Cargo rule comes first in the table.
        assert_eq!(
            detect("target", d.path(), &["Cargo.toml", "pom.xml"]),
            Some(super::ArtifactType::RustTarget)
        );
    }

    #[test]
    fn venv_confirmed_by_inside_marker_or_sibling_config() {
        let d = tmp("venv");
        // `bin/python` actually created inside the candidate.
        std::fs::create_dir_all(d.path().join("bin")).expect("mkdir bin");
        std::fs::write(d.path().join("bin").join("python"), "fake").expect("write python");
        assert_eq!(
            detect::<&str>("venv", d.path(), &[]),
            Some(super::ArtifactType::PythonVenv)
        );
        // No python binary, but a sibling config still confirms.
        let plain = tmp("venvplain");
        assert_eq!(
            detect("venv", plain.path(), &["requirements.txt"]),
            Some(super::ArtifactType::PythonVenv)
        );
        // Nothing confirming: ordinary folder named `env`.
        let empty = tmp("envdir");
        assert_eq!(detect("env", empty.path(), &["readme.md"]), None);
    }

    #[test]
    fn unambiguous_caches_match_without_markers() {
        let d = tmp("cache");
        assert_eq!(
            detect::<&str>("__pycache__", d.path(), &[]),
            Some(super::ArtifactType::PythonCache)
        );
        assert_eq!(
            detect::<&str>(".pytest_cache", d.path(), &[]),
            Some(super::ArtifactType::PythonCache)
        );
        assert_eq!(
            detect::<&str>(".mypy_cache", d.path(), &[]),
            Some(super::ArtifactType::PythonCache)
        );
    }

    #[test]
    fn build_and_gradle_require_java_markers() {
        let d = tmp("gradle");
        assert_eq!(detect("build", d.path(), &["Makefile"]), None);
        assert_eq!(
            detect("build", d.path(), &["build.gradle.kts", "src"]),
            Some(super::ArtifactType::JavaBuild)
        );
        assert_eq!(
            detect(".gradle", d.path(), &["pom.xml", "build.gradle"]),
            Some(super::ArtifactType::JavaBuild)
        );
        // Maven does not produce `build/` or `.gradle/`: pom.xml alone must
        // not make those deletable.
        assert_eq!(detect("build", d.path(), &["pom.xml"]), None);
        assert_eq!(detect(".gradle", d.path(), &["pom.xml"]), None);
    }

    #[test]
    fn bin_obj_require_csproj_or_sln() {
        let d = tmp("dotnet");
        assert_eq!(detect("bin", d.path(), &["Program.cs"]), None);
        assert_eq!(detect("obj", d.path(), &["app.config"]), None);
        assert_eq!(
            detect("bin", d.path(), &["MyApp.csproj"]),
            Some(super::ArtifactType::DotnetBuild)
        );
        assert_eq!(
            detect("obj", d.path(), &["Solution.sln"]),
            Some(super::ArtifactType::DotnetBuild)
        );
        // F# and VB projects are .NET too.
        assert_eq!(
            detect("bin", d.path(), &["MyApp.fsproj"]),
            Some(super::ArtifactType::DotnetBuild)
        );
        assert_eq!(
            detect("obj", d.path(), &["MyApp.vbproj"]),
            Some(super::ArtifactType::DotnetBuild)
        );
        // A C++ solution carries a `.sln` too; the `.vcxproj` rule wins.
        assert_eq!(
            detect(
                "bin",
                d.path(),
                &["OpenCodeInfinite.vcxproj", "OpenCodeInfinite.sln"]
            ),
            Some(super::ArtifactType::CppBuild)
        );
        assert_eq!(
            detect("obj", d.path(), &["App.vcxproj"]),
            Some(super::ArtifactType::CppBuild)
        );
    }

    #[test]
    fn config_prefix_markers_detect_next_and_turbo() {
        let d = tmp("web");
        assert_eq!(
            detect(".next", d.path(), &["next.config.js"]),
            Some(super::ArtifactType::NextCache)
        );
        assert_eq!(
            detect(".nuxt", d.path(), &["package.json"]),
            Some(super::ArtifactType::NuxtCache)
        );
        assert_eq!(
            detect(".turbo", d.path(), &["turbo.json"]),
            Some(super::ArtifactType::TurboCache)
        );
        assert_eq!(detect(".next", d.path(), &["vite.config.js"]), None);
    }

    #[test]
    fn dist_needs_ecosystem_config() {
        let d = tmp("webdist");
        assert_eq!(detect("dist", d.path(), &["main.py"]), None);
        assert_eq!(
            detect("dist", d.path(), &["package.json"]),
            Some(super::ArtifactType::Dist)
        );
        assert_eq!(
            detect("dist", d.path(), &["pyproject.toml"]),
            Some(super::ArtifactType::Dist)
        );
    }

    #[test]
    fn every_rule_has_sane_declaration() {
        // Guard against table misconfigurations slipping in unnoticed.
        for rule in RULES {
            assert!(!rule.name.is_empty());
            assert!(!rule.name.starts_with('/'), "rules are bare names");
        }
    }
}

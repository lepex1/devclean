//! Core data types shared across devclean modules.

use std::path::PathBuf;
use std::time::SystemTime;

/// Ecosystem grouping; maps to the `--type` CLI filter values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Ecosystem {
    /// `node_modules`, `.next`, `.nuxt`, `.turbo`.
    Node,
    /// `target`.
    Rust,
    /// Virtual environments and Python caches.
    Python,
    /// `build`, `.gradle` (Gradle/Maven contexts).
    Java,
    /// `bin`, `obj` (.NET contexts).
    Dotnet,
    /// `dist` (web/app build output).
    Generic,
}

/// Concrete artifact kind produced by the detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactType {
    NodeModules,
    NextCache,
    NuxtCache,
    TurboCache,
    RustTarget,
    PythonVenv,
    PythonCache,
    JavaBuild,
    DotnetBuild,
    CppBuild,
    Dist,
}

impl ArtifactType {
    /// Short human-readable name for the results table.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::NodeModules => "Node.js",
            Self::NextCache => "Next.js",
            Self::NuxtCache => "Nuxt",
            Self::TurboCache => "Turbo",
            Self::RustTarget => "Rust",
            Self::PythonVenv => "Python (venv)",
            Self::PythonCache => "Python",
            Self::JavaBuild => "Gradle/Maven",
            Self::DotnetBuild => ".NET",
            Self::CppBuild => "C++ (VS)",
            Self::Dist => "dist",
        }
    }

    /// Ecosystem bucket used by the `--type` filter.
    #[must_use]
    pub fn ecosystem(self) -> Ecosystem {
        match self {
            Self::NodeModules | Self::NextCache | Self::NuxtCache | Self::TurboCache => {
                Ecosystem::Node
            }
            Self::RustTarget => Ecosystem::Rust,
            Self::PythonVenv | Self::PythonCache => Ecosystem::Python,
            Self::JavaBuild => Ecosystem::Java,
            Self::DotnetBuild => Ecosystem::Dotnet,
            Self::CppBuild | Self::Dist => Ecosystem::Generic,
        }
    }
}

/// A build-artifact directory discovered and measured by the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub path: PathBuf,
    pub artifact_type: ArtifactType,
    pub size_bytes: u64,
    pub last_modified: SystemTime,
}

/// Total reclaimable size of the given artifacts, in bytes.
#[must_use]
pub fn total_size(artifacts: &[Artifact]) -> u64 {
    artifacts.iter().map(|a| a.size_bytes).sum()
}

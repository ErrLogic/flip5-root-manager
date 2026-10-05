//! Discovery of the known, fixed artifact paths inside the (read-only)
//! payload repository (CLAUDE.md section 5).
//!
//! This module only ever *reads* from the payload repository: it checks
//! whether known files exist, stats them, hashes them, and reads git
//! metadata. It never writes, stages, commits, or otherwise mutates the
//! payload repository. It also never substitutes a different artifact for
//! one that is missing — a missing artifact is reported as `Missing`, not
//! silently swapped for another file.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::checksum::sha256_file;

/// Which known artifact this slot refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    PayloadLibrary,
    PayloadRunner,
    KernelSu,
}

impl ArtifactKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            ArtifactKind::PayloadLibrary => "Payload library (cve-2026-43499-app.so)",
            ArtifactKind::PayloadRunner => "Payload runner (cve-2026-43499-root)",
            ArtifactKind::KernelSu => "KernelSU artifact (ksud-b5q-F731BXXS7GZG1-kdp)",
        }
    }

    /// Fixed, known-good relative path within the payload repository.
    /// CLAUDE.md section 5 — these are the only paths this application
    /// will ever look at for each kind; they are never substituted.
    pub fn relative_path(&self) -> &'static str {
        match self {
            ArtifactKind::PayloadLibrary => "build/b5q-F731BXXS7GZG1/cve-2026-43499-app.so",
            ArtifactKind::PayloadRunner => "build/b5q-F731BXXS7GZG1/cve-2026-43499-root",
            ArtifactKind::KernelSu => "kernelsu/ksud-b5q-F731BXXS7GZG1-kdp",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactInfo {
    pub kind: ArtifactKind,
    pub name: &'static str,
    pub relative_path: &'static str,
    pub absolute_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub repository_location: PathBuf,
}

#[derive(Debug, Clone)]
pub enum ArtifactSlot {
    Found(ArtifactInfo),
    Missing {
        kind: ArtifactKind,
        expected_path: PathBuf,
    },
}

impl ArtifactSlot {
    pub fn is_found(&self) -> bool {
        matches!(self, ArtifactSlot::Found(_))
    }
}

/// Handle to the external, read-only payload repository.
#[derive(Debug, Clone)]
pub struct PayloadRepository {
    pub root: PathBuf,
}

impl PayloadRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Looks up a known artifact by kind. Computes its SHA-256 (read-only)
    /// if it is present. Never searches for alternatives if the expected
    /// path is missing.
    pub fn locate(&self, kind: ArtifactKind) -> ArtifactSlot {
        let relative = kind.relative_path();
        let absolute = self.root.join(relative);
        match std::fs::metadata(&absolute) {
            Ok(meta) if meta.is_file() => match sha256_file(&absolute) {
                Ok(sha256) => ArtifactSlot::Found(ArtifactInfo {
                    kind,
                    name: kind.display_name(),
                    relative_path: relative,
                    absolute_path: absolute,
                    size_bytes: meta.len(),
                    sha256,
                    repository_location: self.root.clone(),
                }),
                Err(_) => ArtifactSlot::Missing {
                    kind,
                    expected_path: absolute,
                },
            },
            _ => ArtifactSlot::Missing {
                kind,
                expected_path: absolute,
            },
        }
    }

    pub fn payload_library(&self) -> ArtifactSlot {
        self.locate(ArtifactKind::PayloadLibrary)
    }

    pub fn payload_runner(&self) -> ArtifactSlot {
        self.locate(ArtifactKind::PayloadRunner)
    }

    pub fn kernelsu_artifact(&self) -> ArtifactSlot {
        self.locate(ArtifactKind::KernelSu)
    }

    /// Read-only git metadata about the repository (commit, branch,
    /// whether the working tree is dirty). Never runs a git subcommand
    /// that could mutate the repository.
    pub fn git_info(&self) -> Option<GitInfo> {
        let commit = run_git(&self.root, &["rev-parse", "HEAD"])?;
        let branch = run_git(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let status = run_git(&self.root, &["status", "--porcelain"]).unwrap_or_default();
        Some(GitInfo {
            commit: commit.trim().to_string(),
            branch: branch.trim().to_string(),
            is_dirty: !status.trim().is_empty(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct GitInfo {
    pub commit: String,
    pub branch: String,
    pub is_dirty: bool,
}

/// Runs a single, strictly read-only git inspection subcommand. The caller
/// controls `args`; only `rev-parse` and `status --porcelain` are used
/// anywhere in this codebase, both read-only.
fn run_git(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_fake_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flip5-fakerepo-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("build/b5q-F731BXXS7GZG1")).unwrap();
        std::fs::create_dir_all(dir.join("kernelsu")).unwrap();
        let mut f =
            std::fs::File::create(dir.join("build/b5q-F731BXXS7GZG1/cve-2026-43499-app.so"))
                .unwrap();
        f.write_all(b"fake-so-bytes").unwrap();
        dir
    }

    #[test]
    fn finds_existing_known_artifact() {
        let root = make_fake_repo();
        let repo = PayloadRepository::new(&root);
        let slot = repo.payload_library();
        match slot {
            ArtifactSlot::Found(info) => {
                assert_eq!(
                    info.relative_path,
                    "build/b5q-F731BXXS7GZG1/cve-2026-43499-app.so"
                );
                assert_eq!(info.size_bytes, 13);
                assert!(!info.sha256.is_empty());
            }
            ArtifactSlot::Missing { .. } => panic!("expected artifact to be found"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_missing_artifact_without_substituting() {
        let root = make_fake_repo();
        let repo = PayloadRepository::new(&root);
        // payload runner was never created in the fake repo.
        let slot = repo.payload_runner();
        match slot {
            ArtifactSlot::Missing {
                kind,
                expected_path,
            } => {
                assert_eq!(kind, ArtifactKind::PayloadRunner);
                assert!(expected_path.ends_with("build/b5q-F731BXXS7GZG1/cve-2026-43499-root"));
            }
            ArtifactSlot::Found(_) => panic!("artifact should not exist"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn kernelsu_artifact_missing_is_reported() {
        let root = make_fake_repo();
        let repo = PayloadRepository::new(&root);
        assert!(!repo.kernelsu_artifact().is_found());
        let _ = std::fs::remove_dir_all(&root);
    }
}

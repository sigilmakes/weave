//! Detect Homebrew-owned Weave binaries before the installer update funnel.
//! Homebrew links `<prefix>/bin/wv` into a Cellar formula or a
//! Caskroom cask: resolving the executable symlink is load-bearing because
//! checking only the invoked `bin` path would miss both managed installs.

use std::ffi::OsStr;
use std::path::Path;

/// The Homebrew package layout that owns the running binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomebrewKind {
    /// A formula keg under `Cellar/weave`.
    Formula,
    /// A cask version under `Caskroom/wv`.
    Cask,
}

impl HomebrewKind {
    /// The name of this Homebrew layout.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Formula => "formula",
            Self::Cask => "cask",
        }
    }
}

/// Resolve the executable and identify a Homebrew-managed ancestor pair.
/// Unresolvable paths and unrelated install layouts are not Homebrew-owned.
#[must_use]
pub fn managed_kind(executable: &Path) -> Option<HomebrewKind> {
    let resolved = executable.canonicalize().ok()?;
    for ancestor in resolved.ancestors() {
        let owner = ancestor.parent().and_then(Path::file_name);
        if owner == Some(OsStr::new("Cellar"))
            && ancestor.file_name() == Some(OsStr::new("weave"))
        {
            return Some(HomebrewKind::Formula);
        }
        if owner == Some(OsStr::new("Caskroom"))
            && ancestor.file_name() == Some(OsStr::new("wv"))
        {
            return Some(HomebrewKind::Cask);
        }
    }
    None
}

/// The package-manager command to print instead of attempting self-update.
#[must_use]
pub fn upgrade_instruction(kind: HomebrewKind) -> String {
    match kind {
        HomebrewKind::Formula => "Update with: brew upgrade weave".to_string(),
        HomebrewKind::Cask => "Update with: brew upgrade --cask wv".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[pa_test_scratch::pa_test]
    #[test]
    fn detects_formula_and_cask_layouts() {
        let root = tempfile::tempdir().unwrap();
        let formula = root.path().join("Cellar/weave/1.0.0/libexec/wv");
        let cask = root.path().join("Caskroom/wv/1.0.0/wv");
        std::fs::create_dir_all(formula.parent().unwrap()).unwrap();
        std::fs::create_dir_all(cask.parent().unwrap()).unwrap();
        std::fs::write(&formula, b"binary").unwrap();
        std::fs::write(&cask, b"binary").unwrap();
        assert_eq!(managed_kind(&formula), Some(HomebrewKind::Formula));
        assert_eq!(managed_kind(&cask), Some(HomebrewKind::Cask));
    }

    #[cfg(unix)]
    #[pa_test_scratch::pa_test]
    #[test]
    fn resolves_brew_bin_symlinks_into_the_formula_keg() {
        let root = tempfile::tempdir().unwrap();
        let formula = root.path().join("Cellar/weave/1.0.0/libexec/wv");
        let launcher = root.path().join("bin/wv");
        std::fs::create_dir_all(formula.parent().unwrap()).unwrap();
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&formula, b"binary").unwrap();
        std::os::unix::fs::symlink(&formula, &launcher).unwrap();
        assert_eq!(managed_kind(&launcher), Some(HomebrewKind::Formula));
    }

    #[pa_test_scratch::pa_test]
    #[test]
    fn ignores_other_kegs_and_non_homebrew_installs() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "Cellar/other-tool/1.0.0/libexec/wv",
            "releases/1.0.0-darwin-arm64-abc123/wv",
            "plain/wv",
        ] {
            let executable = root.path().join(path);
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::write(&executable, b"binary").unwrap();
            assert_eq!(managed_kind(&executable), None);
        }
    }
}

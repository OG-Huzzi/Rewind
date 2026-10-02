//! Phase 6: the manifest/lockfile evidence layer
//! (`.ai/PHASE_6_RECIPES.md`, ADR-022).
//!
//! Public claim, and the whole of it: "in this captured command, lockfile
//! `X` (recipe kind `M`) changed from CAS hash `A` (or `absent`) to CAS
//! hash `B` (or `absent`); these recognized manifest paths were present."
//!
//! Recognition is a **pure function of the recorded pre/post manifests**
//! ([`derive_evidence`]): no filesystem access, no scans, no CAS work, no
//! second hashing, no lockfile parsing. The hashes are the manifests'
//! existing `RegularFile` content ids. The module cannot claim which
//! packages or versions changed, and it never speaks about registry,
//! cache, or any other state outside the recorded workspace.

use crate::model::{Fingerprint, Manifest, RecipeEvidence, RecipeKind};

/// One recognized ecosystem: the lockfile name that triggers evidence and
/// the manifest/supporting paths recorded alongside it. Every name here
/// was probed against the real manager on the working host (contract §3);
/// ecosystems without probe evidence stay unrecognized on purpose.
struct Recipe {
    kind: RecipeKind,
    lockfiles: &'static [&'static str],
    manifests: &'static [&'static str],
}

const RECIPES: &[Recipe] = &[
    Recipe {
        kind: RecipeKind::Cargo,
        lockfiles: &["Cargo.lock"],
        manifests: &["Cargo.toml"],
    },
    Recipe {
        kind: RecipeKind::Npm,
        lockfiles: &["package-lock.json", "npm-shrinkwrap.json"],
        manifests: &["package.json"],
    },
    Recipe {
        kind: RecipeKind::Pnpm,
        lockfiles: &["pnpm-lock.yaml"],
        manifests: &["package.json"],
    },
    Recipe {
        kind: RecipeKind::Go,
        lockfiles: &["go.sum"],
        manifests: &["go.mod"],
    },
    Recipe {
        kind: RecipeKind::Uv,
        lockfiles: &["uv.lock"],
        manifests: &["pyproject.toml"],
    },
    Recipe {
        kind: RecipeKind::Pip,
        lockfiles: &["requirements.txt"],
        manifests: &["pyproject.toml", "setup.py"],
    },
];

/// The recorded CAS content id of a recognized lockfile state, or `None`
/// when the path is absent. Only a regular file carries a content id; any
/// other object kind at a lockfile name suppresses the evidence entry —
/// a symlinked or replaced lockfile is never described with a guessed
/// hash.
fn lockfile_hash(manifest: &Manifest, path: &str) -> Option<Option<String>> {
    match manifest.get(path) {
        Fingerprint::Absent => Some(None),
        Fingerprint::RegularFile { content_hash, .. } => Some(Some(content_hash)),
        _ => None,
    }
}

/// Derives the recipe evidence for one captured operation from its
/// recorded pre/post manifests. Pure and side-effect-free: it reads only
/// the given manifests and returns records whose hashes are the recorded
/// CAS ids. Same-hash rewrites produce no entry (explicit no-change
/// policy); a changed manifest without a changed lockfile produces no
/// entry (a manifest edit is not dependency-resolution evidence).
pub fn derive_evidence(before: &Manifest, after: &Manifest) -> Vec<RecipeEvidence> {
    let mut evidence = Vec::new();
    for recipe in RECIPES {
        for lockfile in recipe.lockfiles {
            let Some(pre) = lockfile_hash(before, lockfile) else {
                continue;
            };
            let Some(post) = lockfile_hash(after, lockfile) else {
                continue;
            };
            if pre == post {
                continue;
            }
            let manifests = recipe
                .manifests
                .iter()
                .filter(|path| manifest_has_path(before, path) || manifest_has_path(after, path))
                .map(|path| (*path).to_owned())
                .collect();
            evidence.push(RecipeEvidence {
                kind: recipe.kind,
                lockfile: (*lockfile).to_owned(),
                pre_hash: pre,
                post_hash: post,
                manifests,
            });
        }
    }
    // Lockfile names are unique across recipes by construction; ordering
    // by path is therefore a total order and keeps output deterministic.
    evidence.sort_by(|left, right| left.lockfile.cmp(&right.lockfile));
    evidence
}

/// True when the manifest records any object at this path: the manifest
/// path was "present" in that recorded state. Absent is the only state
/// that counts as not present.
fn manifest_has_path(manifest: &Manifest, path: &str) -> bool {
    !matches!(manifest.get(path), Fingerprint::Absent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MetadataFingerprint;
    use std::collections::BTreeMap;

    fn file(hash: &str) -> Fingerprint {
        Fingerprint::RegularFile {
            content_hash: hash.to_owned(),
            size: 3,
            metadata: MetadataFingerprint::default(),
            streams: BTreeMap::new(),
            dacl: None,
        }
    }

    fn manifest(entries: Vec<(&str, Fingerprint)>) -> Manifest {
        Manifest {
            entries: entries
                .into_iter()
                .map(|(path, fingerprint)| (path.to_owned(), fingerprint))
                .collect(),
        }
    }

    fn directory() -> Fingerprint {
        Fingerprint::Directory {
            manifest_hash: "dirhash".to_owned(),
            entry_count: 0,
            metadata: MetadataFingerprint::default(),
        }
    }

    #[test]
    fn recognizes_all_three_transitions_with_recorded_hashes() {
        let before = manifest(vec![
            ("Cargo.lock", file("aaa")),
            ("Cargo.toml", file("ttt")),
        ]);
        let after = manifest(vec![
            ("Cargo.lock", file("bbb")),
            ("Cargo.toml", file("ttt")),
        ]);
        let evidence = derive_evidence(&before, &after);
        assert_eq!(evidence.len(), 1);
        let entry = &evidence[0];
        assert_eq!(entry.kind, RecipeKind::Cargo);
        assert_eq!(entry.lockfile, "Cargo.lock");
        assert_eq!(entry.pre_hash.as_deref(), Some("aaa"));
        assert_eq!(entry.post_hash.as_deref(), Some("bbb"));
        assert_eq!(entry.manifests, vec!["Cargo.toml".to_owned()]);

        // Added: absent -> present.
        let evidence = derive_evidence(&Manifest::empty(), &after);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].pre_hash, None);
        assert_eq!(evidence[0].post_hash.as_deref(), Some("bbb"));

        // Removed: present -> absent.
        let evidence = derive_evidence(&before, &Manifest::empty());
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].pre_hash.as_deref(), Some("aaa"));
        assert_eq!(evidence[0].post_hash, None);
    }

    #[test]
    fn same_hash_rewrite_is_not_evidence() {
        let before = manifest(vec![
            ("Cargo.lock", file("same")),
            ("Cargo.toml", file("ttt")),
        ]);
        let after = manifest(vec![
            ("Cargo.lock", file("same")),
            ("Cargo.toml", file("ttt")),
        ]);
        assert!(derive_evidence(&before, &after).is_empty());
    }

    #[test]
    fn manifest_only_change_is_not_evidence() {
        let before = manifest(vec![
            ("Cargo.lock", file("aaa")),
            ("Cargo.toml", file("ttt")),
        ]);
        let after = manifest(vec![
            ("Cargo.lock", file("aaa")),
            ("Cargo.toml", file("uuu")),
        ]);
        assert!(derive_evidence(&before, &after).is_empty());
    }

    #[test]
    fn root_only_matching_ignores_nested_and_similar_names() {
        let before = Manifest::empty();
        let after = manifest(vec![
            ("sub/Cargo.lock", file("a")),
            ("node_modules/.package-lock.json", file("b")),
            ("Cargo.lock.bak", file("c")),
            ("package-lock.json.old", file("d")),
            ("vendor/requirements.txt", file("e")),
        ]);
        assert!(derive_evidence(&before, &after).is_empty());
    }

    #[test]
    fn case_deviations_are_not_recognized() {
        let before = Manifest::empty();
        let after = manifest(vec![
            ("cargo.lock", file("a")),
            ("CARGO.LOCK", file("b")),
            ("Package-Lock.json", file("c")),
        ]);
        assert!(derive_evidence(&before, &after).is_empty());
    }

    #[test]
    fn non_regular_lockfile_objects_suppress_evidence() {
        let before = manifest(vec![("Cargo.lock", file("aaa"))]);
        let after = manifest(vec![("Cargo.lock", directory())]);
        assert!(derive_evidence(&before, &after).is_empty());
        let before = manifest(vec![("Cargo.lock", directory())]);
        let after = manifest(vec![("Cargo.lock", file("bbb"))]);
        assert!(derive_evidence(&before, &after).is_empty());
    }

    #[test]
    fn manifests_are_recorded_when_present_in_either_state() {
        // A pip operation that creates requirements.txt while pyproject.toml
        // only existed before: both recorded sides contribute, and absent
        // supporting paths are not listed.
        let before = manifest(vec![("pyproject.toml", file("ppp"))]);
        let after = manifest(vec![("requirements.txt", file("rrr"))]);
        let evidence = derive_evidence(&before, &after);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, RecipeKind::Pip);
        assert_eq!(evidence[0].manifests, vec!["pyproject.toml".to_owned()]);
    }

    #[test]
    fn multiple_ecosystems_are_ordered_by_lockfile_path() {
        let before = Manifest::empty();
        let after = manifest(vec![
            ("uv.lock", file("u")),
            ("Cargo.lock", file("c")),
            ("go.sum", file("g")),
            ("package-lock.json", file("n")),
            ("requirements.txt", file("r")),
            ("pnpm-lock.yaml", file("p")),
        ]);
        let evidence = derive_evidence(&before, &after);
        let paths: Vec<&str> = evidence
            .iter()
            .map(|entry| entry.lockfile.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "Cargo.lock",
                "go.sum",
                "package-lock.json",
                "pnpm-lock.yaml",
                "requirements.txt",
                "uv.lock",
            ]
        );
    }

    #[test]
    fn npm_shrinkwrap_is_npm_lockfile_evidence() {
        let before = manifest(vec![("package-lock.json", file("old"))]);
        let after = manifest(vec![("npm-shrinkwrap.json", file("new"))]);
        let evidence = derive_evidence(&before, &after);
        assert_eq!(evidence.len(), 2);
        // The shrinkwrap add and the lockfile removal are both recorded.
        assert!(evidence.iter().all(|entry| entry.kind == RecipeKind::Npm));
    }
}

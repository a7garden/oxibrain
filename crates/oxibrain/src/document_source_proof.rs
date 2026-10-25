//! Bounded read-only proof for one canonical document in an authorized space.
//! It reuses the document reader/cache and returns no source text or root list.
use crate::{Brain, DocumentFreshness};
use oxibrain_connectors::{
    GitDocumentReader, documents_config::DocumentsConfig, scan::canonicalize_root,
};
use oxibrain_ports::BrainError;
use oxibrain_store::documents::DocumentCache;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentSourceProof {
    pub space: String,
    pub document_uuid: String,
    pub root_alias: String,
    pub canonical_root: String,
    pub locator: String,
    pub source_revision: String,
    pub source_sha256: String,
    pub root_generation: i64,
    pub fresh: bool,
}
fn unavailable() -> BrainError {
    BrainError::Conflict("document source proof unavailable".into())
}
fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}
fn read_source(path: &Path, mut limit: usize) -> Result<Vec<u8>, BrainError> {
    crate::document_plane::read_document_bounded(path, &mut limit).map_err(|_| unavailable())
}

fn fresh_for(freshness: &DocumentFreshness, alias: &str) -> bool {
    freshness.reconciled_roots.iter().any(|root| root == alias)
        && !freshness
            .skipped_roots
            .iter()
            .any(|(root, _)| root == alias)
        && freshness.skipped_files == 0
        && freshness.stale_after_retry.is_empty()
        && !freshness
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.alias == alias)
}
impl Brain {
    /// `space` is an already authorized space ID; surfaces enforce Read and
    /// exact membership before calling. A duplicate UUID in this space, a
    /// stale cache, changed bytes/root, or an ambiguous root fails closed.
    pub async fn document_source_proof(
        &self,
        space: &str,
        document_uuid: &str,
        expected_sha256: &str,
    ) -> Result<Option<DocumentSourceProof>, BrainError> {
        if !valid_uuid(document_uuid)
            || expected_sha256.len() != 64
            || !expected_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(BrainError::Invalid(
                "invalid document source proof identity".into(),
            ));
        }
        if self.config.read_only {
            return Ok(None);
        }
        let before = DocumentsConfig::load(&self.config.dir).map_err(|_| unavailable())?;
        if before.roots.len() > 1024 {
            return Err(unavailable());
        }
        let freshness = self
            .index_documents_for_read()
            .await
            .map_err(|_| unavailable())?;
        let mut eligible = vec![];
        for root in &before.roots {
            let resolved = self
                .lookup_space(&root.space)
                .await?
                .unwrap_or_else(|| root.space.clone());
            if resolved == space {
                eligible.push(root.clone());
            }
        }
        if eligible
            .iter()
            .any(|root| !fresh_for(&freshness, &root.alias))
        {
            return Err(unavailable());
        }
        let dir = self.config.dir.clone();
        let uuid = document_uuid.to_owned();
        let expected = expected_sha256.to_owned();
        let scope = space.to_owned();
        let budget = self.read_policy.materialization_bytes;
        tokio::task::spawn_blocking(move || -> Result<Option<DocumentSourceProof>, BrainError> {
            let cache = match DocumentCache::open_ro(&dir) {
                Ok(cache) => cache,
                Err(BrainError::NotFound(_)) => return Ok(None),
                Err(_) => return Err(unavailable()),
            };
            let mut found = None;
            for root in eligible {
                let Some((id, locator)) = cache.resolve_pdc(&root.alias, &uuid)? else {
                    continue;
                };
                if found.is_some() || !fresh_for(&freshness, &root.alias) {
                    return Err(unavailable());
                }
                if Path::new(&locator)
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
                {
                    return Err(unavailable());
                }
                let Some(meta) = cache.publication_document(&root.alias, &id)? else {
                    return Err(unavailable());
                };
                if meta.space != root.space || meta.locator != locator {
                    return Err(unavailable());
                }
                let canonical = canonicalize_root(&root.path).map_err(|_| unavailable())?;
                let path = canonical
                    .join(&locator)
                    .canonicalize()
                    .map_err(|_| unavailable())?;
                if !path.starts_with(&canonical) {
                    return Err(unavailable());
                }
                let limit = budget.min(usize::try_from(root.max_file_bytes).unwrap_or(usize::MAX));
                let bytes = read_source(&path, limit)?;
                if format!("{:x}", Sha256::digest(&bytes)) != expected {
                    return Ok(None);
                }
                let revision = if meta.revision.starts_with("git:") {
                    let reader = GitDocumentReader::open(&canonical)?.ok_or_else(unavailable)?;
                    reader.current_revision(&reader.snapshot()?, &locator, &bytes)?
                } else {
                    format!("blake3:{}", blake3::hash(&bytes).to_hex())
                };
                if revision != meta.revision {
                    return Err(unavailable());
                }
                let generation = cache.generation(&root.alias)?;
                if generation <= 0 {
                    return Err(unavailable());
                }
                // Repeat byte/config/generation checks before disclosing private
                // path metadata. The proof describes this observed read, never
                // a perpetual authorization or a future filesystem guarantee.
                if read_source(&path, limit)? != bytes
                    || cache.generation(&root.alias)? != generation
                    || DocumentsConfig::load(&dir).map_err(|_| unavailable())? != before
                    || canonicalize_root(&root.path).map_err(|_| unavailable())? != canonical
                {
                    return Err(unavailable());
                }
                found = Some(DocumentSourceProof {
                    space: scope.clone(),
                    document_uuid: uuid.clone(),
                    root_alias: root.alias,
                    canonical_root: canonical.to_string_lossy().into_owned(),
                    locator,
                    source_revision: revision,
                    source_sha256: expected.clone(),
                    root_generation: generation,
                    fresh: true,
                });
            }
            Ok(found)
        })
        .await
        .map_err(|_| unavailable())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrainConfig, DocumentRootSpec};
    const ID: &str = "019a81b4-d4c0-7c00-8000-000000000123";
    fn source(text: &str) -> String {
        format!(
            "---\nformat: pdc-document/2\nbody: pdc-markdown/1\nid: {ID}\ncreated: 2026-10-24T00:00:00.000Z\nupdated: 2026-10-24T00:00:00.000Z\ntitle: Proof fixture\n---\n{text}\n"
        )
    }
    async fn setup() -> (tempfile::TempDir, Brain, String, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let space = brain.ensure_space("proof").await.unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let bytes = source("source marker");
        std::fs::write(root.join(format!("{ID}.md")), &bytes).unwrap();
        brain
            .register_document_root(DocumentRootSpec {
                space: "proof".into(),
                alias: "vault".into(),
                path: root.clone(),
                include: None,
                exclude: None,
                max_file_bytes: None,
            })
            .await
            .unwrap();
        (
            dir,
            brain,
            space,
            root,
            format!("{:x}", Sha256::digest(bytes.as_bytes())),
        )
    }
    #[tokio::test]
    async fn exact_source_proof_binds_authorized_root_locator_hash_revision_and_generation() {
        let (_dir, brain, space, root, hash) = setup().await;
        let proof = brain
            .document_source_proof(&space, ID, &hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            proof.canonical_root,
            root.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(proof.locator, format!("{ID}.md"));
        assert_eq!(proof.source_sha256, hash);
        assert!(proof.source_revision.starts_with("blake3:"));
        assert!(proof.root_generation > 0 && proof.fresh);
        let other = brain.ensure_space("other").await.unwrap();
        assert!(
            brain
                .document_source_proof(&other, ID, &hash)
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn stale_hash_removed_root_and_skipped_source_never_prove_cached_bytes() {
        let (dir, brain, space, root, hash) = setup().await;
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .unwrap()
                .is_some()
        );
        std::fs::write(root.join(format!("{ID}.md")), source("different bytes")).unwrap();
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .unwrap()
                .is_none()
        );
        brain
            .register_document_root(DocumentRootSpec {
                space: "proof".into(),
                alias: "vault".into(),
                path: root,
                include: None,
                exclude: None,
                max_file_bytes: Some(16),
            })
            .await
            .unwrap();
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .is_err()
        );
        std::fs::remove_file(dir.path().join("documents.toml")).unwrap();
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn duplicate_uuid_within_one_root_never_selects_the_first_cached_claim() {
        let (_dir, brain, space, root, hash) = setup().await;
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .unwrap()
                .is_some()
        );
        std::fs::write(root.join("another-claim.md"), source("source marker")).unwrap();
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn duplicate_uuid_across_authorized_roots_is_ambiguous() {
        let (dir, brain, space, _root, hash) = setup().await;
        let second = dir.path().join("second");
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join(format!("{ID}.md")), source("source marker")).unwrap();
        brain
            .register_document_root(DocumentRootSpec {
                space: "proof".into(),
                alias: "second".into(),
                path: second,
                include: None,
                exclude: None,
                max_file_bytes: None,
            })
            .await
            .unwrap();
        assert!(
            brain
                .document_source_proof(&space, ID, &hash)
                .await
                .is_err()
        );
    }
}

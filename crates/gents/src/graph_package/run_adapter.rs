//! Node-bound adapters for bundled graph entry contracts.
//!
//! These adapters prepare host evidence and durable workspace/config rows for
//! the same [`ConfigAccess`] and principal that own the graph. Model-facing
//! tools and the CLI share this code; neither discovers another Gents home,
//! endpoint, credential, or binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use gents_protocol::graphql::graphql_input_literal;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config_client::ConfigAccess;

const EVIDENCE_CHUNKS_PER_PAGE: usize = 16;
const EVIDENCE_CHUNK_MAX_BYTES: usize = 1_800;

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedGraphRun {
    pub entry_name: String,
    pub input: Value,
}

fn git_output_bytes(repo: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()
        .with_context(|| format!("running git in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed in {}: {}",
            arguments.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git_output(repo: &Path, arguments: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_output_bytes(repo, arguments)?)?
        .trim()
        .to_owned())
}

fn git_output_exact(repo: &Path, arguments: &[&str]) -> Result<String> {
    String::from_utf8(git_output_bytes(repo, arguments)?)
        .context("Git emitted non-UTF-8 code-review evidence")
}

fn resolve_repository(
    repo: &Path,
    base: &str,
    head: &str,
    process_root: Option<&Path>,
) -> Result<(PathBuf, String, String)> {
    let canonical = std::fs::canonicalize(repo)
        .with_context(|| format!("canonicalizing repository {}", repo.display()))?;
    crate::workspace::require_under_ceiling(&canonical, process_root)?;
    if git_output(&canonical, &["rev-parse", "--is-inside-work-tree"])? != "true" {
        anyhow::bail!("{} is not a Git work tree", canonical.display());
    }
    // Git discovers enclosing repositories. A permitted subdirectory must not
    // grant review access to the rest of a repository outside the ceiling.
    let repository_root =
        std::fs::canonicalize(git_output(&canonical, &["rev-parse", "--show-toplevel"])?)?;
    crate::workspace::require_under_ceiling(&repository_root, process_root)?;
    let base_sha = git_output(
        &canonical,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?;
    let head_sha = git_output(
        &canonical,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{head}^{{commit}}"),
        ],
    )?;
    Ok((canonical, base_sha, head_sha))
}

struct CodeReviewEvidence {
    summary: String,
    chunks: Vec<String>,
    byte_count: usize,
    sha256: String,
}

fn split_evidence_packet(packet: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < packet.len() {
        let mut end = (start + EVIDENCE_CHUNK_MAX_BYTES).min(packet.len());
        while !packet.is_char_boundary(end) {
            end -= 1;
        }
        chunks.push(packet[start..end].to_owned());
        start = end;
    }
    chunks
}

fn evidence_page_inputs(
    evidence_id: &str,
    evidence_sha256: &str,
    evidence_byte_count: usize,
    chunks: &[String],
) -> Vec<Value> {
    let page_count = chunks.len().div_ceil(EVIDENCE_CHUNKS_PER_PAGE);
    let mut pages = Vec::with_capacity(page_count);
    for page in 0..page_count {
        let first = page * EVIDENCE_CHUNKS_PER_PAGE;
        let mut input = serde_json::Map::new();
        input.insert(
            "page_key".to_owned(),
            Value::String(format!("{evidence_id}:{page:08}")),
        );
        input.insert(
            "evidence_id".to_owned(),
            Value::String(evidence_id.to_owned()),
        );
        input.insert("page_index".to_owned(), Value::String(page.to_string()));
        input.insert(
            "page_count".to_owned(),
            Value::String(page_count.to_string()),
        );
        input.insert(
            "evidence_chunk_count".to_owned(),
            Value::String(chunks.len().to_string()),
        );
        input.insert(
            "evidence_byte_count".to_owned(),
            Value::String(evidence_byte_count.to_string()),
        );
        input.insert(
            "evidence_sha256".to_owned(),
            Value::String(evidence_sha256.to_owned()),
        );
        for slot in 0..EVIDENCE_CHUNKS_PER_PAGE {
            input.insert(
                format!("evidence_chunk_{slot}"),
                Value::String(chunks.get(first + slot).cloned().unwrap_or_default()),
            );
        }
        pages.push(Value::Object(input));
    }
    pages
}

fn code_review_evidence(repo: &Path, base: &str, head: &str) -> Result<CodeReviewEvidence> {
    let changed = git_output(repo, &["diff", "--name-status", base, head, "--"])?;
    let stat = git_output(repo, &["diff", "--stat", base, head, "--"])?;
    let patch = git_output_exact(
        repo,
        &[
            "-c",
            "core.quotepath=true",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "--find-renames=50%",
            "--unified=12",
            base,
            head,
            "--",
        ],
    )?;
    let summary = format!(
        "PINNED BASE: {base}\nPINNED HEAD: {head}\n\nCHANGED FILES:\n{changed}\n\nDIFF STAT:\n{stat}"
    );
    let packet = format!("{summary}\n\nCOMPLETE PATCH:\n{patch}");
    Ok(CodeReviewEvidence {
        summary,
        chunks: split_evidence_packet(&packet),
        byte_count: packet.len(),
        sha256: format!("{:x}", Sha256::digest(packet.as_bytes())),
    })
}

async fn persist_evidence(
    access: &ConfigAccess,
    evidence_id: &str,
    evidence: &CodeReviewEvidence,
) -> Result<()> {
    let pages = evidence_page_inputs(
        evidence_id,
        &evidence.sha256,
        evidence.byte_count,
        &evidence.chunks,
    );
    let manifest = json!({
        "evidence_id": evidence_id,
        "format_version": "1",
        "page_count": evidence.chunks.len().div_ceil(EVIDENCE_CHUNKS_PER_PAGE).to_string(),
        "evidence_chunk_count": evidence.chunks.len().to_string(),
        "evidence_byte_count": evidence.byte_count.to_string(),
        "evidence_sha256": evidence.sha256,
    });
    access
        .transact("graph.prepare_code_review_evidence", move |txn| {
            let manifest = manifest.clone();
            let pages = pages.clone();
            Box::pin(async move {
                txn.execute(&format!(
                    "mutation {{ create_CodeReviewEvidenceManifest(input: {}) {{ _docID }} }}",
                    graphql_input_literal(&manifest)?
                ))
                .await
                .context("persisting immutable code-review evidence manifest")?;
                for (page, input) in pages.iter().enumerate() {
                    txn.execute(&format!(
                        "mutation {{ create_CodeReviewEvidencePage(input: {}) {{ _docID }} }}",
                        graphql_input_literal(input)?
                    ))
                    .await
                    .with_context(|| {
                        format!("persisting immutable code-review evidence page {page}")
                    })?;
                }
                Ok(())
            })
        })
        .await
}

/// Prepare the code-review entry against an explicitly admitted repository.
/// `process_root` is the managed runtime ceiling; passing `None` is reserved
/// for operator CLI callers that already own host authority selection.
pub async fn prepare_code_review_run(
    access: &ConfigAccess,
    principal_did: &str,
    repository: &Path,
    base: &str,
    head: &str,
    focus: Option<String>,
    process_root: Option<&Path>,
) -> Result<PreparedGraphRun> {
    let (repository_path, base_ref, head_ref) =
        resolve_repository(repository, base, head, process_root)?;
    let evidence = code_review_evidence(&repository_path, &base_ref, &head_ref)?;
    let workspace = crate::workspace::provision_read_only_workspace(
        access,
        &repository_path,
        &head_ref,
        principal_did,
    )
    .await?;
    let evidence_id = uuid::Uuid::new_v4().to_string();
    persist_evidence(access, &evidence_id, &evidence).await?;
    Ok(PreparedGraphRun {
        entry_name: "review".to_owned(),
        input: json!({
            "repository_path": ".",
            "base_ref": base_ref,
            "head_ref": head_ref,
            "workspace_id": workspace.workspace.workspace_id,
            "workspace_authority": "readOnly",
            "workspace_owner_agent_did": workspace.workspace.owner_agent_did,
            "lens_count": "4",
            "lens_min": "4",
            "lens_max": "4",
            "pr_number": "",
            "evidence_id": evidence_id,
            "evidence_summary": evidence.summary,
            "evidence_chunk_count": evidence.chunks.len().to_string(),
            "focus": focus.unwrap_or_else(|| "Review the diff for material correctness, safety, durability, and maintainability defects.".to_owned()),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_cannot_escape_ceiling_through_enclosing_repository() {
        let directory = tempfile::tempdir().unwrap();
        git_output(directory.path(), &["init", "--quiet"]).unwrap();
        let permitted = directory.path().join("permitted");
        std::fs::create_dir(&permitted).unwrap();
        let permitted = std::fs::canonicalize(permitted).unwrap();
        let error = resolve_repository(&permitted, "HEAD", "HEAD", Some(&permitted)).unwrap_err();
        assert!(
            error.to_string().contains("escapes operator tool root"),
            "{error:#}"
        );
    }

    #[test]
    fn evidence_pages_are_complete_and_bounded() {
        let packet = format!("{}{}", "a".repeat(1_750_000), "é日".repeat(2_000));
        let chunks = split_evidence_packet(&packet);
        assert_eq!(chunks.concat(), packet);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.len() <= EVIDENCE_CHUNK_MAX_BYTES));
        assert!(EVIDENCE_CHUNK_MAX_BYTES < 2_000);
        let pages = evidence_page_inputs("evidence", "digest", packet.len(), &chunks);
        assert_eq!(pages.len(), chunks.len().div_ceil(EVIDENCE_CHUNKS_PER_PAGE));
        assert!(
            pages.len() > 18,
            "evidence paging must not reintroduce a fixed patch-size ceiling"
        );
        let mut reconstructed = Vec::new();
        for (page, input) in pages.iter().enumerate() {
            let input = input.as_object().unwrap();
            assert_eq!(input.len(), EVIDENCE_CHUNKS_PER_PAGE + 7);
            assert_eq!(input["page_key"], format!("evidence:{page:08}"));
            assert_eq!(input["page_index"], page.to_string());
            assert_eq!(input["page_count"], pages.len().to_string());
            assert_eq!(input["evidence_chunk_count"], chunks.len().to_string());
            assert_eq!(input["evidence_byte_count"], packet.len().to_string());
            assert!(serde_json::to_vec(input).unwrap().len() < 50 * 1024);
            for slot in 0..EVIDENCE_CHUNKS_PER_PAGE {
                let chunk = page * EVIDENCE_CHUNKS_PER_PAGE + slot;
                let value = input[&format!("evidence_chunk_{slot}")].as_str().unwrap();
                if chunk < chunks.len() {
                    reconstructed.push(value.to_owned());
                } else {
                    assert!(value.is_empty(), "only final page padding may be empty");
                }
            }
        }
        assert_eq!(reconstructed.concat(), packet);
    }

    #[test]
    fn empty_evidence_packet_has_no_rows() {
        let chunks = split_evidence_packet("");
        assert!(chunks.is_empty());
        assert!(evidence_page_inputs("empty", "digest", 0, &chunks).is_empty());
    }
}

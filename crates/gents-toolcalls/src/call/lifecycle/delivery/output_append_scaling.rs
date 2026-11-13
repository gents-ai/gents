use crate::call::lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmission, PublishedAdmissionOptions,
};
use gents_session::session::{count_source_payload_bytes, count_source_rows_decoded};
use gents_store::config_client::count_statements;
use gents_store::config_client::session_children::without_child_relationship_repair;

struct Sample {
    statements: usize,
    rows: usize,
    payload_bytes: usize,
    micros: u128,
}

fn grid(variable: &str, default: &str) -> Vec<usize> {
    std::env::var(variable)
        .unwrap_or_else(|_| default.to_owned())
        .split(',')
        .map(|value| value.trim().parse().expect("numeric sweep parameter"))
        .collect()
}

async fn sweep_point(chunks: usize, bytes: usize, repair: bool) {
    let name = format!("append-scaling-{chunks}-{bytes}-{repair}");
    let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: name.clone(),
        ..Default::default()
    })
    .await
    .unwrap();
    let PublishedAdmission {
        node, path, tool, ..
    } = fixture;
    let binding = tool.tool_output_binding().unwrap();
    let chunk = "x".repeat(bytes);

    let mut samples = Vec::with_capacity(chunks);
    for _ in 0..chunks {
        let started = std::time::Instant::now();
        let append = super::append_tool_output(&binding, &chunk);
        let (((appended, payload_bytes), rows), statements) = if repair {
            count_statements(count_source_rows_decoded(count_source_payload_bytes(
                append,
            )))
            .await
        } else {
            without_child_relationship_repair(count_statements(count_source_rows_decoded(
                count_source_payload_bytes(append),
            )))
            .await
        };
        appended.unwrap();
        samples.push(Sample {
            statements: statements.total(),
            rows,
            payload_bytes,
            micros: started.elapsed().as_micros(),
        });
    }

    let report = |label: &str, sample: &Sample| {
        tracing::info!(target: "gents::append_scaling",
            "append_scaling chunks={chunks} bytes={bytes} repair={repair} append={label} \
        statements={} rows={} payload_bytes={} micros={}",
            sample.statements, sample.rows, sample.payload_bytes, sample.micros
        );
    };
    report("first", &samples[0]);
    report("median", &samples[samples.len() / 2]);
    report("last", &samples[samples.len() - 1]);
    tracing::info!(target: "gents::append_scaling",
        "append_scaling chunks={chunks} bytes={bytes} repair={repair} append=total \
    statements={} rows={} payload_bytes={} micros={}",
        samples.iter().map(|s| s.statements).sum::<usize>(),
        samples.iter().map(|s| s.rows).sum::<usize>(),
        samples.iter().map(|s| s.payload_bytes).sum::<usize>(),
        samples.iter().map(|s| s.micros).sum::<u128>(),
    );

    drop(owner);
    node.shutdown().await.expect("shut down test node");
    let _ = tokio::fs::remove_dir_all(path).await;
}

#[tokio::test]
#[ignore = "manual append scaling measurement"]
async fn append_scaling_sweep() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("gents::append_scaling=info")
        .with_test_writer()
        .try_init();
    for chunks in grid("GENTS_APPEND_CHUNKS", "16,64,128,256") {
        for bytes in grid("GENTS_APPEND_BYTES", "256,4096") {
            for repair in [true, false] {
                sweep_point(chunks, bytes, repair).await;
            }
        }
    }
}

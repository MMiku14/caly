//! Real-kernel template-worker E2E: spawns the worker binary and renders a
//! template through `LinuxTemplateWorker`. Skips gracefully when the binary is
//! absent, so an unprivileged `cargo test --all` never fails on this file.

use std::path::PathBuf;
use std::time::Duration;

use caly_domain::BoundedText;
use caly_template::{
    LinuxTemplateWorker, TemplateContext, TemplateRequest, TemplateResponse, TemplateSource,
    TemplateWorker, TemplateWorkerPlan,
};

fn worker_binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_TEMPLATE_WORKER_BIN") {
        return PathBuf::from(value);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/caly-template-worker")
}

fn available() -> bool {
    let binary = worker_binary();
    if !binary.is_file() {
        eprintln!("skipping: no worker binary at {}", binary.display());
        return false;
    }
    true
}

#[test]
fn linux_worker_renders_a_real_template() -> Result<(), Box<dyn std::error::Error>> {
    if !available() {
        return Ok(());
    }
    let source = TemplateSource::try_from_vec(b"node: {{ name }} / {{ latency }} ms".to_vec())
        .map_err(|_| "source")?;
    let context = TemplateContext::try_from_vec(br#"{"name":"hk-edge-01","latency":28}"#.to_vec())
        .map_err(|_| "context")?;
    let plan = TemplateWorkerPlan {
        executable: worker_binary(),
        wall_timeout: Duration::from_secs(5),
        worker_name: BoundedText::new("caly-template-worker-e2e".to_owned()).map_err(|_| "name")?,
    };
    let request = TemplateRequest {
        request_id: [7; 16],
        source,
        context,
    };
    let mut worker = LinuxTemplateWorker;
    let response: TemplateResponse = worker
        .render(&plan, request)
        .map_err(|error| format!("render failed: {error:?}"))?;
    assert_eq!(response.request_id, [7; 16]);
    let output = std::str::from_utf8(response.output.as_slice())?;
    assert_eq!(output, "node: hk-edge-01 / 28 ms");
    Ok(())
}

#[test]
fn linux_worker_reports_render_rejection() -> Result<(), Box<dyn std::error::Error>> {
    if !available() {
        return Ok(());
    }
    let source =
        TemplateSource::try_from_vec(b"{{ bad_syntax }}".to_vec()).map_err(|_| "source")?;
    let context = TemplateContext::try_from_vec(b"{}".to_vec()).map_err(|_| "context")?;
    let plan = TemplateWorkerPlan {
        executable: worker_binary(),
        wall_timeout: Duration::from_secs(5),
        worker_name: BoundedText::new("caly-template-worker-e2e".to_owned()).map_err(|_| "name")?,
    };
    let request = TemplateRequest {
        request_id: [8; 16],
        source,
        context,
    };
    let mut worker = LinuxTemplateWorker;
    let result = worker.render(&plan, request);
    assert!(
        matches!(
            result,
            Err(caly_template::TemplateWorkerFailure::InvalidResponse(_))
        ),
        "expected an invalid-response failure"
    );
    Ok(())
}

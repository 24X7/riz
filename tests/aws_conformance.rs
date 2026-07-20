//! AWS conformance corpus — the strongest "riz runs AWS Lambda" claim there is.
//!
//! This clones AWS's OWN official Lambda samples at pinned commits, builds them,
//! boots them under riz **unmodified**, and asserts they answer. It is the
//! reference implementation's own code, not code we wrote — if riz runs it, riz
//! is AWS-compatible in the way that matters.
//!
//! Scope: the **HTTP / API Gateway v2** subset only. AWS's sample corpus is
//! heavily event-source-driven (SQS / SNS / S3-notification / DynamoDB-stream /
//! EventBridge), which is out of riz's scope by decision; each row records the
//! event shape it targets so that stays explicit.
//!
//! Delivery is pinned live clones, not vendored snapshots: the corpus is AWS's
//! real code at a fixed commit, with zero repo bloat. The trade is a network +
//! toolchain dependency at test time, so this binary **skips cleanly** (never
//! fails) when offline or a required toolchain is missing — the same discipline
//! `template_smoke_all` uses. It is an isolated nextest binary, run on its own
//! like `e2e_smoke_all`, never in the fast suite.
//!
//! Adding a sample is one `Sample` row below. Pin a real commit SHA (never a
//! branch — branches move and break reproducibility).

use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One AWS sample and how to prove it on riz.
struct Sample {
    /// riz function name + scratch-dir label (identifier-safe).
    name: &'static str,
    /// Git repo to clone.
    repo: &'static str,
    /// Pinned commit — immutable and reproducible. NEVER a branch name.
    commit: &'static str,
    /// Path (within the repo) to the example's Cargo manifest. The examples in
    /// the official runtime repo use `path` deps on the workspace crates, so the
    /// whole repo is cloned and the example built in place.
    manifest: &'static str,
    /// Produced binary name under the build target's `release/` dir.
    binary: &'static str,
    /// riz runtime kind for the generated `riz.toml`.
    runtime: &'static str,
    /// The API Gateway event shape the sample targets — documentation + the
    /// scope guard (riz emits v2). Not asserted; recorded so a v1 sample is a
    /// known delta, never a silent pass.
    event_shape: &'static str,
    /// Route to expose and probe.
    method: &'static str,
    path: &'static str,
    /// Expected HTTP status and a substring the body must contain.
    expect_status: u16,
    expect_body: &'static str,
}

/// The pinned corpus. Starts with the official Rust runtime's own `http-basic`
/// example — the same `lambda_http` path a real production Rust lambda
/// (dogfooded separately) runs on. Extend with one row per sample.
const CORPUS: &[Sample] = &[Sample {
    name: "aws_http_basic",
    repo: "https://github.com/awslabs/aws-lambda-rust-runtime.git",
    commit: "8c02edc4b11ba0c68e9c70951ec0519e0351a459",
    manifest: "examples/http-basic-lambda/Cargo.toml",
    binary: "http-basic-lambda",
    runtime: "rust",
    event_shape: "apigw-http-v2",
    method: "GET",
    path: "/",
    expect_status: 200,
    expect_body: "Hello AWS Lambda HTTP request",
}];

#[test]
fn aws_official_samples_run_on_riz_unmodified() {
    if std::env::var("RIZ_SKIP_AWS_CONFORMANCE").is_ok() {
        eprintln!("SKIP: RIZ_SKIP_AWS_CONFORMANCE is set");
        return;
    }
    if !tool_available("git") {
        eprintln!("SKIP: git is not on PATH");
        return;
    }
    if !online() {
        eprintln!("SKIP: offline — cannot reach github.com:443");
        return;
    }

    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let mut ran = 0usize;
    for sample in CORPUS {
        if sample.runtime == "rust" && !tool_available("cargo") {
            eprintln!("SKIP {}: cargo is not on PATH", sample.name);
            continue;
        }
        run_sample(sample, scratch.path());
        ran += 1;
    }
    eprintln!(
        "aws-conformance: {ran}/{} samples verified on riz",
        CORPUS.len()
    );
}

/// Clone → build → boot under riz → assert → (scratch dropped) teardown.
fn run_sample(sample: &Sample, scratch: &Path) {
    let src = scratch.join(format!("{}-src", sample.name));
    let target = scratch.join(format!("{}-target", sample.name));
    let run = scratch.join(format!("{}-run", sample.name));
    std::fs::create_dir_all(&run).expect("run dir");

    // 1. Fetch AWS's real code at the pinned commit. A network failure here is a
    //    SKIP (we already passed the online() probe, but a mid-run blip
    //    shouldn't fail CI); anything else is a real problem worth surfacing.
    if !fetch_pinned(&src, sample.repo, sample.commit) {
        eprintln!(
            "SKIP {}: could not fetch {} @ {}",
            sample.name, sample.repo, sample.commit
        );
        return;
    }

    // 2. Build the sample UNMODIFIED, into an isolated target dir (cleaned with
    //    the scratch tempdir). A build failure is a real conformance failure.
    let manifest = src.join(sample.manifest);
    let build = Command::new("cargo")
        .args(["build", "--release", "--manifest-path"])
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "4")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .expect("spawn cargo build");
    assert!(
        build.success(),
        "{}: AWS sample failed to build",
        sample.name
    );

    let built = target.join("release").join(sample.binary);
    assert!(
        built.exists(),
        "{}: expected binary {built:?} not produced",
        sample.name
    );
    let handler = run.join(sample.binary);
    std::fs::copy(&built, &handler).expect("copy built binary into run dir");

    // 3. Generate a riz.toml pointing at the unmodified binary and boot riz.
    let port = free_port();
    std::fs::write(run.join("riz.toml"), riz_toml(sample, port)).expect("write riz.toml");
    let mut child = Command::new(env!("CARGO_BIN_EXE_riz"))
        .current_dir(&run)
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn riz");
    let _reaper = Reaper(&mut child);

    assert!(
        wait_for_ready(port, Duration::from_secs(20)),
        "{}: riz did not become ready",
        sample.name
    );

    // 4. Hit the AWS handler through riz and assert it answered.
    let url = format!("http://127.0.0.1:{port}{}", sample.path);
    let client = reqwest::blocking::Client::new();
    let method = reqwest::Method::from_bytes(sample.method.as_bytes()).expect("method");
    let resp = client
        .request(method, &url)
        .send()
        .unwrap_or_else(|e| panic!("{}: request to {url} failed: {e}", sample.name));
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    assert_eq!(
        status, sample.expect_status,
        "{}: status {status} != {} (body: {body})",
        sample.name, sample.expect_status
    );
    assert!(
        body.contains(sample.expect_body),
        "{}: body {body:?} did not contain {:?}",
        sample.name,
        sample.expect_body
    );
    eprintln!(
        "PASS {} [{}]: {} {} -> {} ({} bytes) — AWS's own code, unmodified",
        sample.name,
        sample.event_shape,
        sample.method,
        sample.path,
        status,
        body.len()
    );
}

/// Generate a minimal riz.toml exposing the sample's single route.
fn riz_toml(sample: &Sample, port: u16) -> String {
    format!(
        r#"[server]
port = {port}
host = "127.0.0.1"

[function.{name}]
runtime = "{runtime}"
handler = "./{binary}"
timeout_ms = 10000
concurrency = 1

[[function.{name}.routes]]
path = "{path}"
method = "{method}"
"#,
        name = sample.name,
        runtime = sample.runtime,
        binary = sample.binary,
        path = sample.path,
        method = sample.method,
    )
}

/// Fetch exactly the pinned commit (GitHub serves any reachable SHA), shallow.
fn fetch_pinned(dir: &Path, repo: &str, commit: &str) -> bool {
    std::fs::create_dir_all(dir).expect("src dir");
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    git(&["init", "-q"])
        && git(&["remote", "add", "origin", repo])
        && git(&["fetch", "--depth", "1", "origin", commit])
        && git(&["checkout", "-q", "FETCH_HEAD"])
}

/// Kill the riz child on drop so a failing assertion never leaks a process
/// (nextest's leak detector would fail the run otherwise).
struct Reaper<'a>(&'a mut std::process::Child);
impl Drop for Reaper<'_> {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn online() -> bool {
    "github.com:443"
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
        .unwrap_or(false)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind 0")
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_ready(port: u16, deadline: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/ready");
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(r) = reqwest::blocking::get(&url) {
            if r.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

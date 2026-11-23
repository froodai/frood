//! A stand-in for the Kubernetes API server and its kubelet, for tests, with no cluster.
//!
//! It keeps Jobs, Secrets and PersistentVolumeClaims of one namespace in memory, and refuses
//! requests without the module's token. When a Job is let go it "runs" its pod: the image is a
//! local program (the fake agent), the claim a folder that becomes `$HOME`, the env comes from
//! the Secrets the pod refers to and `$(VAR)` in args is expanded as Kubernetes does. stdout
//! and stderr go to one log, as in Kubernetes. Pods that break the `restricted` Pod Security
//! Standard are refused, as a locked-down namespace would.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub const NAMESPACE: &str = "agents-ns";
pub const TOKEN: &str = "sa-token";

pub struct Pod {
    pub job: String,
    pub object: Value,
    child: Option<std::process::Child>,
    log: PathBuf,
    /// A waiting reason it will not get out of (an image that cannot be pulled).
    stuck: Option<String>,
    exit: Option<i64>,
}

#[derive(Default)]
pub struct World {
    pub claims: BTreeMap<String, Value>,
    pub jobs: BTreeMap<String, Value>,
    pub secrets: BTreeMap<String, Value>,
    pub pods: BTreeMap<String, Pod>,
    /// `METHOD plural[/name[/sub]]` of every request, in order.
    pub requests: Vec<String>,
    /// Kinds the module may not create (RBAC says no).
    pub forbidden: Vec<String>,
    root: PathBuf,
    uids: u64,
}

impl World {
    pub fn claim_dir(&self, name: &str) -> PathBuf {
        self.root.join("volumes").join(name)
    }

    /// The Job runs over its deadline: Kubernetes kills its pod and marks it failed.
    pub fn exceed_deadline(&mut self, job: &str) {
        for pod in self.pods.values_mut().filter(|p| p.job == job) {
            if let Some(mut child) = pod.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            pod.exit = Some(137);
        }
        if let Some(job) = self.jobs.get_mut(job) {
            job["status"]["conditions"] = json!([{ "type": "Failed", "status": "True", "reason": "DeadlineExceeded", "message": "Job was active longer than specified deadline" }]);
        }
    }

    fn pod_status(pod: &mut Pod) -> Value {
        if let Some(reason) = &pod.stuck {
            return json!({ "phase": "Pending", "containerStatuses": [{ "name": "agent", "state": { "waiting": { "reason": reason, "message": "Back-off pulling image" } } }] });
        }
        if pod.exit.is_none()
            && let Some(child) = pod.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            pod.exit = Some(status.code().unwrap_or(-1).into());
        }
        match pod.exit {
            Some(code) => json!({
                "phase": if code == 0 { "Succeeded" } else { "Failed" },
                "initContainerStatuses": [{ "name": "home", "state": { "terminated": { "exitCode": 0 } } }],
                "containerStatuses": [{ "name": "agent", "state": { "terminated": { "exitCode": code } } }],
            }),
            None => {
                json!({ "phase": "Running", "containerStatuses": [{ "name": "agent", "state": { "running": {} } }] })
            }
        }
    }

    fn pod_object(pod: &mut Pod) -> Value {
        let mut object = pod.object.clone();
        object["status"] = Self::pod_status(pod);
        object
    }

    fn job_with_status(&mut self, name: &str) -> Option<Value> {
        let mut job = self.jobs.get(name)?.clone();
        if job["status"]["conditions"].is_array() {
            return Some(job);
        }
        for pod in self.pods.values_mut().filter(|p| p.job == name) {
            let status = Self::pod_status(pod);
            if let Some(code) = pod.exit {
                job["status"]["conditions"] = if code == 0 {
                    json!([{ "type": "Complete", "status": "True" }])
                } else {
                    json!([{ "type": "Failed", "status": "True", "reason": "BackoffLimitExceeded", "message": "Job has reached the specified backoff limit" }])
                };
            } else if status["phase"] == "Running" {
                job["status"]["active"] = json!(1);
            }
        }
        Some(job)
    }

    /// The kubelet's part: the pod of a Job that was let go.
    fn start_pod(&mut self, job_name: &str) -> Result<(), String> {
        let job = self.jobs[job_name].clone();
        let spec = &job["spec"]["template"]["spec"];
        let claim = spec["volumes"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|v| v["persistentVolumeClaim"]["claimName"].as_str())
            .ok_or("the pod has no claim")?
            .to_owned();
        if !self.claims.contains_key(&claim) {
            return Err(format!("persistentvolumeclaim {claim} not found"));
        }
        let home = self.claim_dir(&claim);
        let name = format!("{job_name}-{:05}", self.pods.len());
        let log = self.root.join(format!("{name}.log"));
        let mut labels = job["spec"]["template"]["metadata"]["labels"].clone();
        labels["job-name"] = json!(job_name);
        let object = json!({ "metadata": { "name": name, "labels": labels, "creationTimestamp": "2026-11-23T00:00:00Z" } });
        let container = &spec["containers"][0];
        let image = container["image"].as_str().unwrap_or_default().to_owned();
        let mut pod = Pod {
            job: job_name.to_owned(),
            object,
            child: None,
            log: log.clone(),
            stuck: None,
            exit: None,
        };
        if image.starts_with("missing/") {
            pod.stuck = Some("ImagePullBackOff".into());
            self.pods.insert(name, pod);
            return Ok(());
        }
        // The init container makes the working folder in the claim.
        let init = &spec["initContainers"][0]["command"];
        assert_eq!(*init, json!(["mkdir", "-p", "/home/agent/work"]));
        std::fs::create_dir_all(home.join("work")).map_err(|e| e.to_string())?;
        let mut env: Vec<(String, String)> = Vec::new();
        for var in container["env"].as_array().into_iter().flatten() {
            let key = var["name"].as_str().unwrap_or_default().to_owned();
            let value = match var["valueFrom"]["secretKeyRef"].as_object() {
                Some(reference) => {
                    let secret = reference["name"].as_str().unwrap_or_default();
                    let key = reference["key"].as_str().unwrap_or_default();
                    self.secrets
                        .get(secret)
                        .and_then(|s| s["stringData"][key].as_str())
                        .ok_or_else(|| {
                            format!("CreateContainerConfigError: secret {secret} key {key}")
                        })?
                        .to_owned()
                }
                None => expand(var["value"].as_str().unwrap_or_default(), &env),
            };
            env.push((key, value));
        }
        let args: Vec<String> = container["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| expand(a.as_str().unwrap_or_default(), &env))
            .collect();
        let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let child = std::process::Command::new(&image)
            .args(&args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .envs(env.iter().map(|(k, v)| (k, v)))
            // The claim is mounted at the agent's home.
            .env("HOME", &home)
            .current_dir(home.join("work"))
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| e.to_string())?;
        pod.child = Some(child);
        self.pods.insert(name, pod);
        Ok(())
    }

    fn delete_job(&mut self, name: &str) -> bool {
        let Some(job) = self.jobs.remove(name) else {
            return false;
        };
        let uid = job["metadata"]["uid"].clone();
        let pods: Vec<String> = self
            .pods
            .iter()
            .filter(|(_, p)| p.job == name)
            .map(|(n, _)| n.clone())
            .collect();
        for pod in pods {
            if let Some(mut pod) = self.pods.remove(&pod)
                && let Some(mut child) = pod.child.take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        // The garbage collector takes what the Job owned.
        self.secrets
            .retain(|_, s| s["metadata"]["ownerReferences"][0]["uid"] != uid);
        true
    }
}

/// `$(VAR)` from the variables before it, `$$` for a `$`, as Kubernetes expands args and env.
fn expand(text: &str, env: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(inner) = after.strip_prefix('(')
            && let Some(end) = inner.find(')')
        {
            let name = &inner[..end];
            match env.iter().rev().find(|(k, _)| k == name) {
                Some((_, value)) => out.push_str(value),
                None => out.push_str(&format!("$({name})")),
            }
            rest = &inner[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Breaks of the `restricted` Pod Security Standard in a pod template.
fn not_restricted(pod: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let pod_security = &pod["securityContext"];
    for container in ["initContainers", "containers"]
        .iter()
        .flat_map(|list| pod[*list].as_array().cloned().unwrap_or_default())
    {
        let name = container["name"].as_str().unwrap_or("?").to_owned();
        let security = &container["securityContext"];
        if security["allowPrivilegeEscalation"] != false {
            problems.push(format!("{name}: allowPrivilegeEscalation != false"));
        }
        if security["privileged"] == true {
            problems.push(format!("{name}: privileged"));
        }
        let drops = security["capabilities"]["drop"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !drops.contains(&json!("ALL")) {
            problems.push(format!("{name}: capabilities must drop ALL"));
        }
        if security["runAsNonRoot"] != true && pod_security["runAsNonRoot"] != true {
            problems.push(format!("{name}: runAsNonRoot != true"));
        }
        if security["runAsUser"] == 0 || pod_security["runAsUser"] == 0 {
            problems.push(format!("{name}: runAsUser=0"));
        }
        let seccomp = if security["seccompProfile"].is_object() {
            &security["seccompProfile"]["type"]
        } else {
            &pod_security["seccompProfile"]["type"]
        };
        if seccomp != "RuntimeDefault" && seccomp != "Localhost" {
            problems.push(format!("{name}: seccompProfile"));
        }
    }
    for volume in pod["volumes"].as_array().into_iter().flatten() {
        if volume.get("hostPath").is_some() {
            problems.push("hostPath volumes".into());
        }
    }
    if pod["hostNetwork"] == true || pod["hostPID"] == true || pod["hostIPC"] == true {
        problems.push("host namespaces".into());
    }
    problems
}

fn status(code: StatusCode, message: impl Into<String>) -> Response {
    let reason = match code {
        StatusCode::NOT_FOUND => "NotFound",
        StatusCode::CONFLICT => "AlreadyExists",
        StatusCode::FORBIDDEN => "Forbidden",
        StatusCode::UNAUTHORIZED => "Unauthorized",
        _ => "BadRequest",
    };
    (
        code,
        axum::Json(json!({ "kind": "Status", "apiVersion": "v1", "status": "Failure", "message": message.into(), "reason": reason, "code": code.as_u16() })),
    )
        .into_response()
}

type Shared = Arc<Mutex<World>>;

async fn handle(
    State(world): State<Shared>,
    method: Method,
    uri: Uri,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOKEN}"))
    {
        return status(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    let parts: Vec<&str> = uri.path().trim_matches('/').split('/').collect();
    let rest = match parts.as_slice() {
        ["api", "v1", "namespaces", ns, rest @ ..]
        | ["apis", "batch", "v1", "namespaces", ns, rest @ ..] => {
            if *ns != NAMESPACE {
                return status(StatusCode::FORBIDDEN, format!("namespace {ns} is not ours"));
            }
            rest.to_vec()
        }
        _ => return status(StatusCode::NOT_FOUND, "no such path"),
    };
    let mut world = world.lock().unwrap();
    world.requests.push(format!("{method} {}", rest.join("/")));
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let name_of = |body: &Value| {
        body["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    match (method.as_str(), rest.as_slice()) {
        ("POST", [plural]) if world.forbidden.iter().any(|f| f == plural) => status(
            StatusCode::FORBIDDEN,
            format!(
                "{plural} is forbidden: User \"system:serviceaccount:{NAMESPACE}:frood-agents\" cannot create resource \"{plural}\""
            ),
        ),
        ("POST", ["persistentvolumeclaims"]) => {
            let name = name_of(&body);
            if world.claims.contains_key(&name) {
                return status(
                    StatusCode::CONFLICT,
                    format!("persistentvolumeclaims \"{name}\" already exists"),
                );
            }
            std::fs::create_dir_all(world.claim_dir(&name)).unwrap();
            world.claims.insert(name, body.clone());
            (StatusCode::CREATED, axum::Json(body)).into_response()
        }
        ("DELETE", ["persistentvolumeclaims", name]) => {
            assert_eq!(
                query.get("propagationPolicy").map(String::as_str),
                Some("Background")
            );
            if world.claims.remove(*name).is_none() {
                return status(
                    StatusCode::NOT_FOUND,
                    format!("persistentvolumeclaims \"{name}\" not found"),
                );
            }
            let _ = std::fs::remove_dir_all(world.claim_dir(name));
            axum::Json(json!({ "kind": "Status", "status": "Success" })).into_response()
        }
        ("POST", ["jobs"]) => {
            let name = name_of(&body);
            if name.len() > 63 {
                return status(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "the Job's name is longer than 63 characters",
                );
            }
            let problems = not_restricted(&body["spec"]["template"]["spec"]);
            if !problems.is_empty() {
                return status(
                    StatusCode::FORBIDDEN,
                    format!(
                        "violates PodSecurity \"restricted:latest\": {}",
                        problems.join(", ")
                    ),
                );
            }
            if world.jobs.contains_key(&name) {
                return status(
                    StatusCode::CONFLICT,
                    format!("jobs.batch \"{name}\" already exists"),
                );
            }
            world.uids += 1;
            let mut job = body.clone();
            job["metadata"]["uid"] = json!(format!("uid-{}", world.uids));
            job["status"] = json!({});
            world.jobs.insert(name.clone(), job.clone());
            if job["spec"]["suspend"] != true
                && let Err(err) = world.start_pod(&name)
            {
                return status(StatusCode::BAD_REQUEST, err);
            }
            (StatusCode::CREATED, axum::Json(job)).into_response()
        }
        ("GET", ["jobs", name]) => match world.job_with_status(name) {
            Some(job) => axum::Json(job).into_response(),
            None => status(
                StatusCode::NOT_FOUND,
                format!("jobs.batch \"{name}\" not found"),
            ),
        },
        ("PATCH", ["jobs", name]) => {
            assert_eq!(
                headers.get("content-type").and_then(|v| v.to_str().ok()),
                Some("application/merge-patch+json")
            );
            let Some(job) = world.jobs.get_mut(*name) else {
                return status(
                    StatusCode::NOT_FOUND,
                    format!("jobs.batch \"{name}\" not found"),
                );
            };
            let was = job["spec"]["suspend"] == true;
            if let Some(suspend) = body["spec"]["suspend"].as_bool() {
                job["spec"]["suspend"] = json!(suspend);
            }
            let job = job.clone();
            if was && job["spec"]["suspend"] == false {
                // A missing Secret keeps the container from starting.
                if let Err(err) = world.start_pod(name) {
                    panic!("the pod could not start: {err}");
                }
            }
            axum::Json(job).into_response()
        }
        ("DELETE", ["jobs", name]) => {
            assert_eq!(
                query.get("propagationPolicy").map(String::as_str),
                Some("Background"),
                "without it, the Job's pods would stay"
            );
            if world.delete_job(name) {
                axum::Json(json!({ "kind": "Status", "status": "Success" })).into_response()
            } else {
                status(
                    StatusCode::NOT_FOUND,
                    format!("jobs.batch \"{name}\" not found"),
                )
            }
        }
        ("POST", ["secrets"]) => {
            let name = name_of(&body);
            let owner = &body["metadata"]["ownerReferences"][0];
            let owned = world.jobs.values().any(|j| {
                j["metadata"]["uid"] == owner["uid"] && j["metadata"]["name"] == owner["name"]
            });
            assert!(owned, "the turn's Secret belongs to its Job: {body}");
            if world.secrets.contains_key(&name) {
                return status(
                    StatusCode::CONFLICT,
                    format!("secrets \"{name}\" already exists"),
                );
            }
            world.secrets.insert(name, body.clone());
            (
                StatusCode::CREATED,
                axum::Json(json!({ "metadata": body["metadata"] })),
            )
                .into_response()
        }
        ("GET", ["pods"]) => {
            let selector = query.get("labelSelector").cloned().unwrap_or_default();
            let job = selector
                .strip_prefix("job-name=")
                .unwrap_or_default()
                .to_owned();
            let items: Vec<Value> = world
                .pods
                .values_mut()
                .filter(|p| p.job == job)
                .map(World::pod_object)
                .collect();
            axum::Json(json!({ "kind": "PodList", "items": items })).into_response()
        }
        ("GET", ["pods", name]) => match world.pods.get_mut(*name) {
            Some(pod) => axum::Json(World::pod_object(pod)).into_response(),
            None => status(StatusCode::NOT_FOUND, format!("pods \"{name}\" not found")),
        },
        ("GET", ["pods", name, "log"]) => {
            assert_eq!(query.get("container").map(String::as_str), Some("agent"));
            let Some(pod) = world.pods.get(*name) else {
                return status(StatusCode::NOT_FOUND, format!("pods \"{name}\" not found"));
            };
            if pod.stuck.is_some() {
                return status(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "container \"agent\" in pod \"{name}\" is waiting to start: trying and failing to pull image"
                    ),
                );
            }
            let text = std::fs::read_to_string(&pod.log).unwrap_or_default();
            let text = match query.get("tailLines").and_then(|t| t.parse::<usize>().ok()) {
                Some(tail) => {
                    let lines: Vec<&str> = text.split_inclusive('\n').collect();
                    lines[lines.len().saturating_sub(tail)..].concat()
                }
                None => text,
            };
            text.into_response()
        }
        _ => status(
            StatusCode::NOT_FOUND,
            format!("{method} {} is not handled", uri.path()),
        ),
    }
}

pub struct Cluster {
    pub url: String,
    pub world: Shared,
}

impl Cluster {
    pub async fn start(root: &Path) -> Self {
        let world = Arc::new(Mutex::new(World {
            root: root.to_owned(),
            ..Default::default()
        }));
        let app = Router::new().fallback(handle).with_state(world.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, world }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_expand_like_kubernetes() {
        let env = vec![("A".to_owned(), "x $(B) $$".to_owned())];
        assert_eq!(
            expand("$(A)", &env),
            "x $(B) $$",
            "values are not expanded again"
        );
        assert_eq!(expand("$$(A) $(C) $5", &env), "$(A) $(C) $5");
    }
}

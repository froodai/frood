//! Where agent turns run: one Kubernetes Job per turn, a PersistentVolumeClaim per run for the
//! agent's home (its sessions live there, so a later turn resumes them).
//!
//! A turn's command line and environment carry the run's credentials and run token, so they
//! are not written into the Job: they go into a Secret of that turn, owned by the Job, and the
//! container refers to them (`$(FROOD_ARG_0)` in its args, `secretKeyRef` in its env). Kubernetes
//! deletes the Secret with the Job. The Job is created suspended, the Secret next, and then the
//! Job is let go, so its pod never starts without its Secret.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;

use serde_json::{Value, json};

use crate::kube::{Kind, Kube};

/// Home of the agent inside its container; the run's volume is mounted here.
pub const AGENT_HOME: &str = "/home/agent";
/// The agent's container in the turn's pod.
pub const CONTAINER: &str = "agent";

pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub volume: String,
    pub labels: HashMap<String, String>,
    /// The turn ends after this many seconds, whatever the agent does.
    pub deadline_secs: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub enum Exit {
    Running,
    Exited(i64),
    /// The turn ended without the agent's exit code: it could not start, or Kubernetes
    /// stopped it (deadline, eviction). Says why.
    Failed(String),
    /// The Job is not there any more.
    Gone,
}

pub trait Backend: Send + Sync + 'static {
    fn create_volume(&self, name: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn remove_volume(&self, name: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Starts a turn; returns its id.
    fn run(&self, spec: &ContainerSpec) -> impl Future<Output = Result<String, String>> + Send;
    /// Everything the agent printed to stdout so far.
    fn stdout(&self, id: &str) -> impl Future<Output = Result<String, String>> + Send;
    /// The last lines it printed to stderr.
    fn stderr_tail(&self, id: &str) -> impl Future<Output = Result<String, String>> + Send;
    fn state(&self, id: &str) -> impl Future<Output = Result<Exit, String>> + Send;
    /// Stops and removes the turn; not an error when it is gone.
    fn remove(&self, id: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Removes every turn of the run that is still there, including ones the module lost track
    /// of (it went down between creating a Job and saving the run).
    fn remove_run_turns(&self, run_id: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// Limits and placement of agent pods.
#[derive(Clone, Debug)]
pub struct Placement {
    pub cpus: Option<f64>,
    pub memory_mb: Option<i64>,
    pub session_mb: i64,
    pub storage_class: Option<String>,
    pub read_only_root: bool,
    pub run_as_user: i64,
    pub service_account: Option<String>,
    pub image_pull_secrets: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub node_selector: BTreeMap<String, String>,
    pub keep_finished_minutes: u64,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            cpus: None,
            memory_mb: None,
            session_mb: 1024,
            storage_class: None,
            read_only_root: true,
            run_as_user: 10001,
            service_account: None,
            image_pull_secrets: Vec::new(),
            labels: BTreeMap::new(),
            node_selector: BTreeMap::new(),
            keep_finished_minutes: 1440,
        }
    }
}

/// Labels on everything the module creates, so a person can find (and a NetworkPolicy select)
/// the agents.
fn base_labels(spec_labels: &HashMap<String, String>) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, String> = spec_labels
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    labels.insert("app.kubernetes.io/name".into(), "frood-agent".into());
    labels.insert(
        "app.kubernetes.io/managed-by".into(),
        "frood-runtime-k8s".into(),
    );
    labels
}

/// The env var a turn's argument `n` travels in.
fn arg_var(n: usize) -> String {
    format!("FROOD_ARG_{n}")
}

/// A pod that runs the agent as an unprivileged user on a read-only root, with nothing from the
/// cluster: no API token, no service links.
pub fn job_manifest(spec: &ContainerSpec, placement: &Placement) -> Value {
    let labels = base_labels(&spec.labels);
    let mut pod_labels = labels.clone();
    pod_labels.extend(placement.labels.clone());
    let mut resources = serde_json::Map::new();
    if let Some(cpus) = placement.cpus {
        resources.insert(
            "cpu".into(),
            json!(format!("{}m", (cpus * 1000.0).round() as i64)),
        );
    }
    if let Some(memory) = placement.memory_mb {
        resources.insert("memory".into(), json!(format!("{memory}Mi")));
    }
    // Requests equal limits: namespaces with a quota often require both.
    let resources = if resources.is_empty() {
        json!({})
    } else {
        json!({ "limits": resources, "requests": resources })
    };
    let user = placement.run_as_user;
    let container_security = json!({
        "runAsNonRoot": true,
        "runAsUser": user,
        "runAsGroup": user,
        "allowPrivilegeEscalation": false,
        "privileged": false,
        "readOnlyRootFilesystem": placement.read_only_root,
        "capabilities": { "drop": ["ALL"] },
        "seccompProfile": { "type": "RuntimeDefault" },
    });
    let mut env: Vec<Value> = spec
        .env
        .iter()
        .map(|(name, _)| {
            json!({ "name": name, "valueFrom": { "secretKeyRef": { "name": spec.name, "key": name } } })
        })
        .collect();
    env.extend((0..spec.args.len()).map(|n| {
        json!({ "name": arg_var(n), "valueFrom": { "secretKeyRef": { "name": spec.name, "key": arg_var(n) } } })
    }));
    env.push(json!({ "name": "HOME", "value": AGENT_HOME }));
    let args: Vec<String> = (0..spec.args.len())
        .map(|n| format!("$({})", arg_var(n)))
        .collect();
    let mounts = json!([
        { "name": "home", "mountPath": AGENT_HOME },
        { "name": "tmp", "mountPath": "/tmp" },
    ]);
    let mut pod = json!({
        "restartPolicy": "Never",
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "terminationGracePeriodSeconds": 10,
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": user,
            "runAsGroup": user,
            "fsGroup": user,
            // Only a new volume needs its owner changed, not every file on every turn.
            "fsGroupChangePolicy": "OnRootMismatch",
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        // A new volume is empty: make the agent's working folder in it first, as the user.
        "initContainers": [{
            "name": "home",
            "image": spec.image,
            "command": ["mkdir", "-p", format!("{AGENT_HOME}/work")],
            "securityContext": container_security,
            "resources": resources,
            "volumeMounts": mounts,
        }],
        "containers": [{
            "name": CONTAINER,
            "image": spec.image,
            "args": args,
            "env": env,
            "workingDir": format!("{AGENT_HOME}/work"),
            "securityContext": container_security,
            "resources": resources,
            "volumeMounts": mounts,
        }],
        "volumes": [
            { "name": "home", "persistentVolumeClaim": { "claimName": spec.volume } },
            { "name": "tmp", "emptyDir": { "sizeLimit": "256Mi" } },
        ],
    });
    if let Some(account) = &placement.service_account {
        pod["serviceAccountName"] = json!(account);
    }
    if !placement.image_pull_secrets.is_empty() {
        pod["imagePullSecrets"] = placement
            .image_pull_secrets
            .iter()
            .map(|name| json!({ "name": name }))
            .collect();
    }
    if !placement.node_selector.is_empty() {
        pod["nodeSelector"] = json!(placement.node_selector);
    }
    let mut job = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": { "name": spec.name, "labels": labels },
        "spec": {
            // Started once the Secret is there; never retried: a failed turn is the run's.
            "suspend": true,
            "backoffLimit": 0,
            "ttlSecondsAfterFinished": placement.keep_finished_minutes * 60,
            "template": { "metadata": { "labels": pod_labels }, "spec": pod },
        },
    });
    if let Some(deadline) = spec.deadline_secs {
        job["spec"]["activeDeadlineSeconds"] = json!(deadline);
    }
    job
}

/// The turn's command line and environment, owned by its Job.
pub fn secret_manifest(spec: &ContainerSpec, job_uid: &str) -> Value {
    let mut data: BTreeMap<String, String> = spec.env.iter().cloned().collect();
    for (n, arg) in spec.args.iter().enumerate() {
        data.insert(arg_var(n), arg.clone());
    }
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "Opaque",
        "metadata": {
            "name": spec.name,
            "labels": base_labels(&spec.labels),
            "ownerReferences": [{
                "apiVersion": "batch/v1",
                "kind": "Job",
                "name": spec.name,
                "uid": job_uid,
            }],
        },
        "stringData": data,
    })
}

pub fn claim_manifest(name: &str, placement: &Placement) -> Value {
    let mut claim = json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": name,
            "labels": base_labels(&HashMap::from([("frood.run".to_owned(), "true".to_owned())])),
        },
        "spec": {
            "accessModes": ["ReadWriteOnce"],
            "resources": { "requests": { "storage": format!("{}Mi", placement.session_mb) } },
        },
    });
    if let Some(class) = &placement.storage_class {
        claim["spec"]["storageClassName"] = json!(class);
    }
    claim
}

/// Waiting reasons after which the agent's container will not start by itself.
const STUCK: &[&str] = &[
    "ImagePullBackOff",
    "InvalidImageName",
    "ErrImageNeverPull",
    "CreateContainerConfigError",
    "CreateContainerError",
    "RunContainerError",
];

fn container_status<'a>(pod: &'a Value, list: &str, name: &str) -> Option<&'a Value> {
    pod["status"][list]
        .as_array()?
        .iter()
        .find(|s| s["name"] == name)
}

fn reason_of(value: &Value) -> String {
    let reason = value["reason"].as_str().unwrap_or_default();
    let message = value["message"].as_str().unwrap_or_default();
    match (reason.is_empty(), message.is_empty()) {
        (false, false) => format!("{reason}: {message}"),
        (false, true) => reason.to_owned(),
        (true, false) => message.to_owned(),
        (true, true) => "no reason given".to_owned(),
    }
}

/// How a turn stands, from its Job and its pod.
pub fn exit_of(job: &Value, pod: Option<&Value>) -> Exit {
    let failed = job["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["type"] == "Failed" && c["status"] == "True");
    // Over its deadline the Job stops the pod: its exit code would only say "killed".
    if let Some(condition) = failed
        && condition["reason"] == "DeadlineExceeded"
    {
        return Exit::Failed(format!(
            "the turn ran out of time: {}",
            reason_of(condition)
        ));
    }
    if let Some(pod) = pod {
        if let Some(init) = container_status(pod, "initContainerStatuses", "home")
            && let Some(code) = init["state"]["terminated"]["exitCode"].as_i64()
            && code != 0
        {
            return Exit::Failed(format!(
                "cannot prepare the agent's home (exit code {code}): {}",
                reason_of(&init["state"]["terminated"])
            ));
        }
        for list in ["initContainerStatuses", "containerStatuses"] {
            for status in pod["status"][list].as_array().into_iter().flatten() {
                let waiting = &status["state"]["waiting"];
                if let Some(reason) = waiting["reason"].as_str()
                    && STUCK.contains(&reason)
                {
                    return Exit::Failed(format!("cannot start the agent: {}", reason_of(waiting)));
                }
            }
        }
        if let Some(agent) = container_status(pod, "containerStatuses", CONTAINER)
            && let Some(code) = agent["state"]["terminated"]["exitCode"].as_i64()
        {
            return Exit::Exited(code);
        }
        if pod["status"]["phase"] == "Failed" {
            return Exit::Failed(format!(
                "Kubernetes stopped the agent: {}",
                reason_of(&pod["status"])
            ));
        }
    }
    if let Some(condition) = failed {
        return Exit::Failed(format!("the agent's job failed: {}", reason_of(condition)));
    }
    let complete = job["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["type"] == "Complete" && c["status"] == "True");
    if complete {
        // Its pod is gone, but the Job says it succeeded.
        return Exit::Exited(0);
    }
    Exit::Running
}

/// Lines that are not the agent's stream-json: what it printed to stderr, as far as one can
/// tell in a log that has both.
pub fn stderr_lines(log: &str, last: usize) -> String {
    let lines: Vec<&str> = log
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty()
                && serde_json::from_str::<serde_json::Map<String, Value>>(line).is_err()
        })
        .collect();
    lines[lines.len().saturating_sub(last)..].join("\n")
}

pub struct KubeBackend {
    kube: Kube,
    placement: Placement,
}

impl KubeBackend {
    pub fn new(kube: Kube, placement: Placement) -> Self {
        Self { kube, placement }
    }

    /// The turn's pod, by its Job's label: the module may list pods, not get one by name.
    async fn pod(&self, job: &str) -> Result<Option<Value>, String> {
        let pods = self
            .kube
            .list(Kind::Pod, &format!("job-name={job}"))
            .await
            .map_err(|e| e.to_string())?;
        // backoffLimit is 0: one pod, unless one was replaced before it started.
        Ok(pods.into_iter().max_by_key(|p| {
            p["metadata"]["creationTimestamp"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        }))
    }

    async fn log(&self, job: &str, tail: Option<u32>) -> Result<String, String> {
        let Some(pod) = self.pod(job).await? else {
            return Ok(String::new());
        };
        let name = pod["metadata"]["name"].as_str().unwrap_or_default();
        self.kube
            .logs(name, CONTAINER, tail)
            .await
            .map(Option::unwrap_or_default)
            .map_err(|e| e.to_string())
    }
}

impl Backend for KubeBackend {
    async fn create_volume(&self, name: &str) -> Result<(), String> {
        match self
            .kube
            .create(Kind::Claim, &claim_manifest(name, &self.placement))
            .await
        {
            Err(err) if !err.conflict() => Err(err.to_string()),
            _ => Ok(()),
        }
    }

    async fn remove_volume(&self, name: &str) -> Result<(), String> {
        self.kube
            .delete(Kind::Claim, name)
            .await
            .map_err(|e| e.to_string())
    }

    async fn run(&self, spec: &ContainerSpec) -> Result<String, String> {
        let job = match self
            .kube
            .create(Kind::Job, &job_manifest(spec, &self.placement))
            .await
        {
            Ok(job) => job,
            // A Job of this name from an attempt the module lost track of (it went down before
            // saving the run, or lost its state). It is not this turn's: it goes, with its
            // Secret, and the next attempt starts clean.
            Err(err) if err.conflict() => {
                tracing::warn!(job = %spec.name, "removing a leftover job of the same name");
                self.kube
                    .delete(Kind::Job, &spec.name)
                    .await
                    .map_err(|e| e.to_string())?;
                return Err(format!(
                    "job {} was left from an earlier attempt and is being removed; try again",
                    spec.name
                ));
            }
            Err(err) => return Err(err.to_string()),
        };
        let uid = job["metadata"]["uid"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let started = async {
            self.kube
                .create(Kind::Secret, &secret_manifest(spec, &uid))
                .await?;
            self.kube
                .merge_patch(
                    Kind::Job,
                    &spec.name,
                    &json!({ "spec": { "suspend": false } }),
                )
                .await
        }
        .await;
        if let Err(err) = started {
            let _ = self.kube.delete(Kind::Job, &spec.name).await;
            return Err(err.to_string());
        }
        Ok(spec.name.clone())
    }

    async fn stdout(&self, id: &str) -> Result<String, String> {
        self.log(id, None).await
    }

    async fn stderr_tail(&self, id: &str) -> Result<String, String> {
        Ok(stderr_lines(&self.log(id, Some(200)).await?, 20))
    }

    async fn state(&self, id: &str) -> Result<Exit, String> {
        let Some(job) = self
            .kube
            .get(Kind::Job, id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(Exit::Gone);
        };
        let pod = self.pod(id).await?;
        Ok(exit_of(&job, pod.as_ref()))
    }

    async fn remove(&self, id: &str) -> Result<(), String> {
        self.kube
            .delete(Kind::Job, id)
            .await
            .map_err(|e| e.to_string())
    }

    async fn remove_run_turns(&self, run_id: &str) -> Result<(), String> {
        self.kube
            .delete_all(Kind::Job, &run_selector(run_id))
            .await
            .map_err(|e| e.to_string())
    }
}

/// The Jobs of one run.
pub fn run_selector(run_id: &str) -> String {
    format!("frood.run={run_id},app.kubernetes.io/managed-by=frood-runtime-k8s")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ContainerSpec {
        ContainerSpec {
            name: "frood-run-0199-1".into(),
            image: "ghcr.io/froodai/agent-claude-code:0.1.15".into(),
            args: vec!["-p".into(), "Look at $(HOME) and $$ money".into()],
            env: vec![("ANTHROPIC_API_KEY".into(), "sk-test".into())],
            volume: "frood-run-0199".into(),
            labels: HashMap::from([("frood.run".to_owned(), "0199".to_owned())]),
            deadline_secs: Some(960),
        }
    }

    #[test]
    fn agent_pods_are_locked_down_and_carry_no_secret_in_the_job() {
        let placement = Placement {
            cpus: Some(0.5),
            memory_mb: Some(2048),
            labels: BTreeMap::from([("app.kubernetes.io/instance".into(), "work".into())]),
            image_pull_secrets: vec!["registry".into()],
            ..Default::default()
        };
        let job = job_manifest(&spec(), &placement);
        let text = job.to_string();
        assert!(
            !text.contains("sk-test"),
            "the key is not in the Job: {text}"
        );
        assert!(!text.contains("money"), "nor the command line");
        assert_eq!(job["spec"]["suspend"], true);
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 960);
        assert_eq!(job["spec"]["ttlSecondsAfterFinished"], 86400);
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(pod["automountServiceAccountToken"], false);
        assert_eq!(pod["restartPolicy"], "Never");
        assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
        assert_eq!(pod["securityContext"]["fsGroup"], 10001);
        assert_eq!(pod["imagePullSecrets"][0]["name"], "registry");
        assert!(pod.get("serviceAccountName").is_none());
        let labels = &job["spec"]["template"]["metadata"]["labels"];
        assert_eq!(labels["app.kubernetes.io/name"], "frood-agent");
        assert_eq!(labels["app.kubernetes.io/instance"], "work");
        assert_eq!(labels["frood.run"], "0199");
        for container in [&pod["initContainers"][0], &pod["containers"][0]] {
            let security = &container["securityContext"];
            assert_eq!(security["allowPrivilegeEscalation"], false);
            assert_eq!(security["readOnlyRootFilesystem"], true);
            assert_eq!(security["capabilities"]["drop"][0], "ALL");
            assert_eq!(security["runAsUser"], 10001);
            assert_eq!(container["resources"]["limits"]["cpu"], "500m");
            assert_eq!(container["resources"]["requests"]["memory"], "2048Mi");
            assert_eq!(container["volumeMounts"][0]["mountPath"], AGENT_HOME);
        }
        let agent = &pod["containers"][0];
        assert_eq!(agent["args"], json!(["$(FROOD_ARG_0)", "$(FROOD_ARG_1)"]));
        assert_eq!(agent["workingDir"], "/home/agent/work");
        let env = agent["env"].as_array().unwrap();
        assert_eq!(
            env[0]["valueFrom"]["secretKeyRef"],
            json!({ "name": "frood-run-0199-1", "key": "ANTHROPIC_API_KEY" })
        );
        assert!(
            env.iter()
                .any(|e| e["name"] == "HOME" && e["value"] == AGENT_HOME)
        );

        let secret = secret_manifest(&spec(), "uid-1");
        assert_eq!(secret["metadata"]["ownerReferences"][0]["uid"], "uid-1");
        assert_eq!(secret["metadata"]["ownerReferences"][0]["kind"], "Job");
        // Kubernetes does not expand references inside a value from a Secret.
        assert_eq!(
            secret["stringData"]["FROOD_ARG_1"],
            "Look at $(HOME) and $$ money"
        );
        assert_eq!(secret["stringData"]["ANTHROPIC_API_KEY"], "sk-test");

        let open = Placement {
            read_only_root: false,
            service_account: Some("agents".into()),
            storage_class: Some("fast".into()),
            ..Default::default()
        };
        let mut no_deadline = spec();
        no_deadline.deadline_secs = None;
        let job = job_manifest(&no_deadline, &open);
        assert!(job["spec"].get("activeDeadlineSeconds").is_none());
        let pod = &job["spec"]["template"]["spec"];
        assert_eq!(
            pod["containers"][0]["securityContext"]["readOnlyRootFilesystem"],
            false
        );
        assert_eq!(pod["serviceAccountName"], "agents");
        assert_eq!(pod["containers"][0]["resources"], json!({}));
        let claim = claim_manifest("frood-run-0199", &open);
        assert_eq!(claim["spec"]["storageClassName"], "fast");
        assert_eq!(claim["spec"]["resources"]["requests"]["storage"], "1024Mi");
        assert_eq!(claim["spec"]["accessModes"][0], "ReadWriteOnce");
    }

    #[test]
    fn the_job_and_its_pod_say_how_the_turn_stands() {
        let job = json!({ "status": {} });
        let pod = |status: Value| json!({ "metadata": { "name": "p" }, "status": status });
        assert_eq!(exit_of(&job, None), Exit::Running);
        assert_eq!(
            exit_of(
                &job,
                Some(&pod(
                    json!({ "phase": "Pending", "containerStatuses": [{ "name": "agent", "state": { "waiting": { "reason": "ContainerCreating" } } }] })
                ))
            ),
            Exit::Running
        );
        assert_eq!(
            exit_of(
                &job,
                Some(&pod(
                    json!({ "containerStatuses": [{ "name": "agent", "state": { "terminated": { "exitCode": 3 } } }] })
                ))
            ),
            Exit::Exited(3)
        );
        let Exit::Failed(why) = exit_of(
            &job,
            Some(&pod(
                json!({ "containerStatuses": [{ "name": "agent", "state": { "waiting": { "reason": "ImagePullBackOff", "message": "Back-off pulling image \"x\"" } } }] }),
            )),
        ) else {
            panic!("a pod that cannot pull its image fails");
        };
        assert!(why.contains("ImagePullBackOff"), "{why}");
        let Exit::Failed(why) = exit_of(
            &job,
            Some(&pod(
                json!({ "initContainerStatuses": [{ "name": "home", "state": { "terminated": { "exitCode": 1, "reason": "Error" } } }] }),
            )),
        ) else {
            panic!("a home that cannot be made fails");
        };
        assert!(why.contains("home"), "{why}");
        let late = json!({ "status": { "conditions": [{ "type": "Failed", "status": "True", "reason": "DeadlineExceeded", "message": "Job was active longer than specified deadline" }] } });
        let Exit::Failed(why) = exit_of(
            &late,
            Some(&pod(
                json!({ "containerStatuses": [{ "name": "agent", "state": { "terminated": { "exitCode": 137 } } }] }),
            )),
        ) else {
            panic!("a turn over its deadline fails");
        };
        assert!(why.contains("ran out of time"), "{why}");
        let evicted =
            pod(json!({ "phase": "Failed", "reason": "Evicted", "message": "low on memory" }));
        assert_eq!(
            exit_of(&job, Some(&evicted)),
            Exit::Failed("Kubernetes stopped the agent: Evicted: low on memory".into())
        );
        let done =
            json!({ "status": { "conditions": [{ "type": "Complete", "status": "True" }] } });
        assert_eq!(exit_of(&done, None), Exit::Exited(0));
    }

    #[test]
    fn stderr_is_what_is_not_stream_json() {
        let log = "{\"type\":\"system\"}\nboom\n{\"type\":\"result\"}\nError: no key\n";
        assert_eq!(stderr_lines(log, 20), "boom\nError: no key");
        assert_eq!(stderr_lines(log, 1), "Error: no key");
    }
}

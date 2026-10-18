//! Where agent turns run: one container per turn, a volume per run for the agent's home (its
//! sessions live there, so a later turn resumes them).

use std::collections::HashMap;
use std::future::Future;

use bollard::Docker;
use bollard::models::{
    ContainerCreateBody, ContainerStateStatusEnum, HostConfig, Mount, MountType,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    RemoveVolumeOptionsBuilder,
};
use futures_util::StreamExt;

/// Home of the agent inside its container; the run's volume is mounted here.
pub const AGENT_HOME: &str = "/home/agent";

pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub volume: String,
    pub labels: HashMap<String, String>,
}

#[derive(Debug, PartialEq)]
pub enum Exit {
    Running,
    Exited(i64),
    /// The container is not there any more.
    Gone,
}

pub trait Backend: Send + Sync + 'static {
    fn create_volume(&self, name: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn remove_volume(&self, name: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Starts a container; returns its id.
    fn run(&self, spec: &ContainerSpec) -> impl Future<Output = Result<String, String>> + Send;
    /// Everything the container printed to stdout so far.
    fn stdout(&self, id: &str) -> impl Future<Output = Result<String, String>> + Send;
    /// The last lines it printed to stderr.
    fn stderr_tail(&self, id: &str) -> impl Future<Output = Result<String, String>> + Send;
    fn state(&self, id: &str) -> impl Future<Output = Result<Exit, String>> + Send;
    /// Stops and removes the container; not an error when it is gone.
    fn remove(&self, id: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// Limits and placement of agent containers.
#[derive(Clone, Default)]
pub struct Placement {
    /// Docker network the containers join, to reach Frood and the sources.
    pub network: Option<String>,
    pub cpus: Option<f64>,
    pub memory_mb: Option<i64>,
}

pub struct DockerBackend {
    docker: Docker,
    placement: Placement,
}

impl DockerBackend {
    pub fn connect(placement: Placement) -> Result<Self, String> {
        let docker = Docker::connect_with_local_defaults().map_err(|e| e.to_string())?;
        Ok(Self { docker, placement })
    }

    async fn logs(&self, id: &str, stdout: bool, tail: &str) -> Result<String, String> {
        let options = LogsOptionsBuilder::new()
            .stdout(stdout)
            .stderr(!stdout)
            .tail(tail)
            .build();
        let mut stream = self.docker.logs(id, Some(options));
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            out.extend_from_slice(&chunk.into_bytes());
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

fn not_found(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

impl Backend for DockerBackend {
    async fn create_volume(&self, name: &str) -> Result<(), String> {
        let request = bollard::models::VolumeCreateRequest {
            name: Some(name.to_owned()),
            labels: Some(HashMap::from([("frood.run".to_owned(), "true".to_owned())])),
            ..Default::default()
        };
        self.docker
            .create_volume(request)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn remove_volume(&self, name: &str) -> Result<(), String> {
        match self
            .docker
            .remove_volume(
                name,
                Some(RemoveVolumeOptionsBuilder::new().force(true).build()),
            )
            .await
        {
            Err(err) if !not_found(&err) => Err(err.to_string()),
            _ => Ok(()),
        }
    }

    async fn run(&self, spec: &ContainerSpec) -> Result<String, String> {
        let placement = &self.placement;
        let host = HostConfig {
            mounts: Some(vec![Mount {
                typ: Some(MountType::VOLUME),
                source: Some(spec.volume.clone()),
                target: Some(AGENT_HOME.to_owned()),
                ..Default::default()
            }]),
            network_mode: placement.network.clone(),
            nano_cpus: placement.cpus.map(|c| (c * 1e9) as i64),
            memory: placement.memory_mb.map(|m| m * 1024 * 1024),
            pids_limit: Some(512),
            cap_drop: Some(vec!["ALL".into()]),
            security_opt: Some(vec!["no-new-privileges".into()]),
            readonly_rootfs: Some(true),
            tmpfs: Some(HashMap::from([("/tmp".to_owned(), "size=256m".to_owned())])),
            // Reaches a Frood server that runs on the host (Linux needs this mapping).
            extra_hosts: Some(vec!["host.docker.internal:host-gateway".into()]),
            ..Default::default()
        };
        let mut env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        env.push(format!("HOME={AGENT_HOME}"));
        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: Some(spec.args.clone()),
            env: Some(env),
            working_dir: Some(format!("{AGENT_HOME}/work")),
            labels: Some(spec.labels.clone()),
            host_config: Some(host),
            ..Default::default()
        };
        let created = self
            .docker
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&spec.name)
                        .build(),
                ),
                body,
            )
            .await
            .map_err(|e| e.to_string())?;
        if let Err(err) = self.docker.start_container(&created.id, None).await {
            let _ = self.remove(&created.id).await;
            return Err(err.to_string());
        }
        Ok(created.id)
    }

    async fn stdout(&self, id: &str) -> Result<String, String> {
        self.logs(id, true, "all").await
    }

    async fn stderr_tail(&self, id: &str) -> Result<String, String> {
        self.logs(id, false, "20").await
    }

    async fn state(&self, id: &str) -> Result<Exit, String> {
        match self.docker.inspect_container(id, None).await {
            Ok(info) => {
                let state = info.state.unwrap_or_default();
                Ok(match state.status {
                    Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD) => {
                        Exit::Exited(state.exit_code.unwrap_or(-1))
                    }
                    _ => Exit::Running,
                })
            }
            Err(err) if not_found(&err) => Ok(Exit::Gone),
            Err(err) => Err(err.to_string()),
        }
    }

    async fn remove(&self, id: &str) -> Result<(), String> {
        match self
            .docker
            .remove_container(
                id,
                Some(RemoveContainerOptionsBuilder::new().force(true).build()),
            )
            .await
        {
            Err(err) if !not_found(&err) => Err(err.to_string()),
            _ => Ok(()),
        }
    }
}

/// Runs the "image" as a local program, with a directory as its home: for tests, with a fake
/// agent script in place of an image.
#[cfg(test)]
pub mod process {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    pub struct ProcessBackend {
        pub root: PathBuf,
        children: Mutex<HashMap<String, std::process::Child>>,
    }

    impl ProcessBackend {
        pub fn new(root: PathBuf) -> Self {
            Self {
                root,
                children: Mutex::new(HashMap::new()),
            }
        }

        pub fn volume_dir(&self, name: &str) -> PathBuf {
            self.root.join("volumes").join(name)
        }

        fn out(&self, id: &str, stream: &str) -> PathBuf {
            self.root.join(format!("{id}.{stream}"))
        }
    }

    impl Backend for ProcessBackend {
        async fn create_volume(&self, name: &str) -> Result<(), String> {
            std::fs::create_dir_all(self.volume_dir(name).join("work")).map_err(|e| e.to_string())
        }

        async fn remove_volume(&self, name: &str) -> Result<(), String> {
            let _ = std::fs::remove_dir_all(self.volume_dir(name));
            Ok(())
        }

        async fn run(&self, spec: &ContainerSpec) -> Result<String, String> {
            let home = self.volume_dir(&spec.volume);
            let id = spec.name.clone();
            let stdout =
                std::fs::File::create(self.out(&id, "stdout")).map_err(|e| e.to_string())?;
            let stderr =
                std::fs::File::create(self.out(&id, "stderr")).map_err(|e| e.to_string())?;
            let child = std::process::Command::new(&spec.image)
                .args(&spec.args)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", &home)
                .envs(spec.env.iter().map(|(k, v)| (k, v)))
                .current_dir(home.join("work"))
                .stdout(stdout)
                .stderr(stderr)
                .spawn()
                .map_err(|e| e.to_string())?;
            self.children.lock().unwrap().insert(id.clone(), child);
            Ok(id)
        }

        async fn stdout(&self, id: &str) -> Result<String, String> {
            Ok(std::fs::read_to_string(self.out(id, "stdout")).unwrap_or_default())
        }

        async fn stderr_tail(&self, id: &str) -> Result<String, String> {
            Ok(std::fs::read_to_string(self.out(id, "stderr")).unwrap_or_default())
        }

        async fn state(&self, id: &str) -> Result<Exit, String> {
            let mut children = self.children.lock().unwrap();
            let Some(child) = children.get_mut(id) else {
                return Ok(Exit::Gone);
            };
            Ok(match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => Exit::Exited(status.code().unwrap_or(-1).into()),
                None => Exit::Running,
            })
        }

        async fn remove(&self, id: &str) -> Result<(), String> {
            if let Some(mut child) = self.children.lock().unwrap().remove(id) {
                let _ = child.kill();
                let _ = child.wait();
            }
            Ok(())
        }
    }
}

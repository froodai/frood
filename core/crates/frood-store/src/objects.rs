//! Where files are kept (docs/26 §2): a folder, an S3-compatible storage, or the database
//! itself. A file's key is the SHA-256 of its bytes, so the same file is kept once.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ObjectsError(String);

/// The store files go to.
#[derive(Clone)]
pub enum Objects {
    /// Files stay in the database (`blobs.data`).
    Database,
    Disk {
        root: PathBuf,
    },
    S3(S3),
}

impl Objects {
    pub fn is_database(&self) -> bool {
        matches!(self, Self::Database)
    }

    /// A short name for logs and health: `database`, `disk` or `s3`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Disk { .. } => "disk",
            Self::S3(_) => "s3",
        }
    }

    pub async fn put(&self, key: &str, data: &[u8]) -> Result<(), ObjectsError> {
        check_key(key)?;
        match self {
            Self::Database => Err(ObjectsError("files are kept in the database".into())),
            Self::Disk { root } => {
                let path = disk_path(root, key);
                if tokio::fs::try_exists(&path).await.unwrap_or(false) {
                    return Ok(());
                }
                let dir = path.parent().expect("a key has a folder");
                tokio::fs::create_dir_all(dir).await.map_err(io(dir))?;
                // Written aside, then moved: a file is either whole or not there.
                let partial = path.with_extension("partial");
                tokio::fs::write(&partial, data)
                    .await
                    .map_err(io(&partial))?;
                tokio::fs::rename(&partial, &path).await.map_err(io(&path))
            }
            Self::S3(s3) => s3.put(key, data).await,
        }
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, ObjectsError> {
        check_key(key)?;
        match self {
            Self::Database => Err(ObjectsError("files are kept in the database".into())),
            Self::Disk { root } => {
                let path = disk_path(root, key);
                tokio::fs::read(&path).await.map_err(io(&path))
            }
            Self::S3(s3) => s3.get(key).await,
        }
    }
}

fn check_key(key: &str) -> Result<(), ObjectsError> {
    if key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ObjectsError(format!("{key:?} is not a file's SHA-256")))
    }
}

/// `root/ab/abcdef…`: a few thousand files per folder at most.
fn disk_path(root: &Path, key: &str) -> PathBuf {
    root.join(&key[..2]).join(key)
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> ObjectsError + '_ {
    move |err| ObjectsError(format!("{}: {err}", path.display()))
}

/// An S3-compatible storage, reached with AWS Signature Version 4.
#[derive(Clone)]
pub struct S3 {
    client: reqwest::Client,
    endpoint: url::Url,
    bucket: String,
    region: String,
    prefix: String,
    access_key_id: String,
    secret_access_key: String,
    path_style: bool,
}

impl S3 {
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: Option<&str>,
        prefix: Option<&str>,
        access_key_id: &str,
        secret_access_key: &str,
        path_style: bool,
    ) -> Result<Self, ObjectsError> {
        let endpoint = url::Url::parse(endpoint)
            .map_err(|e| ObjectsError(format!("endpoint {endpoint:?}: {e}")))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15 * 60))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ObjectsError(e.to_string()))?;
        Ok(Self {
            client,
            endpoint,
            bucket: bucket.to_owned(),
            region: region.unwrap_or("us-east-1").to_owned(),
            prefix: prefix.unwrap_or_default().to_owned(),
            access_key_id: access_key_id.to_owned(),
            secret_access_key: secret_access_key.to_owned(),
            path_style,
        })
    }

    /// The object's address: `endpoint/bucket/key` or `bucket.endpoint/key`.
    fn url(&self, key: &str) -> Result<url::Url, ObjectsError> {
        let object = format!("{}{key}", self.prefix);
        let mut url = self.endpoint.clone();
        if self.path_style {
            url.set_path(&format!("/{}/{object}", self.bucket));
        } else {
            let host = format!("{}.{}", self.bucket, url.host_str().unwrap_or_default());
            url.set_host(Some(&host))
                .map_err(|e| ObjectsError(format!("bucket host {host:?}: {e}")))?;
            url.set_path(&format!("/{object}"));
        }
        Ok(url)
    }

    async fn put(&self, key: &str, data: &[u8]) -> Result<(), ObjectsError> {
        let url = self.url(key)?;
        let request = self.signed(reqwest::Method::PUT, &url, data, chrono::Utc::now());
        let response = request
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| ObjectsError(e.without_url().to_string()))?;
        check(response).await.map(|_| ())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, ObjectsError> {
        let url = self.url(key)?;
        let response = self
            .signed(reqwest::Method::GET, &url, b"", chrono::Utc::now())
            .send()
            .await
            .map_err(|e| ObjectsError(e.without_url().to_string()))?;
        let response = check(response).await?;
        Ok(response
            .bytes()
            .await
            .map_err(|e| ObjectsError(e.without_url().to_string()))?
            .to_vec())
    }

    fn signed(
        &self,
        method: reqwest::Method,
        url: &url::Url,
        payload: &[u8],
        at: chrono::DateTime<chrono::Utc>,
    ) -> reqwest::RequestBuilder {
        let (_, authorization, hash, stamp) = self.sign(method.as_str(), url, payload, at);
        self.client
            .request(method, url.clone())
            .header("x-amz-date", stamp)
            .header("x-amz-content-sha256", hash)
            .header("authorization", authorization)
    }

    /// Signature Version 4 of a request with no query and these headers: `host`,
    /// `x-amz-content-sha256`, `x-amz-date`. Returns the day, the authorization, the payload's
    /// hash and the time stamp.
    fn sign(
        &self,
        method: &str,
        url: &url::Url,
        payload: &[u8],
        at: chrono::DateTime<chrono::Utc>,
    ) -> (String, String, String, String) {
        let stamp = at.format("%Y%m%dT%H%M%SZ").to_string();
        let day = at.format("%Y%m%d").to_string();
        let hash = hex::encode(Sha256::digest(payload));
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let canonical = format!(
            "{method}\n{path}\n\nhost:{host}\nx-amz-content-sha256:{hash}\nx-amz-date:{stamp}\n\n\
             host;x-amz-content-sha256;x-amz-date\n{hash}",
            path = url.path(),
        );
        let scope = format!("{day}/{}/s3/aws4_request", self.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        );
        let key = [self.region.as_bytes(), b"s3", b"aws4_request"]
            .iter()
            .fold(
                hmac(
                    format!("AWS4{}", self.secret_access_key).as_bytes(),
                    day.as_bytes(),
                ),
                |key, part| hmac(&key, part),
            );
        let signature = hex::encode(hmac(&key, to_sign.as_bytes()));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}",
            self.access_key_id
        );
        (day, authorization, hash, stamp)
    }
}

async fn check(response: reqwest::Response) -> Result<reqwest::Response, ObjectsError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    // S3 answers in XML: the code and message are what a person needs.
    let field = |name: &str| {
        let open = format!("<{name}>");
        let start = body.find(&open)? + open.len();
        let end = start + body[start..].find('<')?;
        Some(body[start..end].to_owned())
    };
    Err(ObjectsError(format!(
        "the storage answered {status}{}",
        field("Code")
            .map(|code| format!(": {code} {}", field("Message").unwrap_or_default()))
            .unwrap_or_default()
    )))
}

/// HMAC-SHA256 (RFC 2104).
fn hmac(key: &[u8], message: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex::encode(hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn requests_are_signed_as_aws_documents() {
        // The GET Object example of the S3 Signature Version 4 documentation, without its
        // Range header.
        let s3 = S3::new(
            "https://s3.amazonaws.com",
            "examplebucket",
            None,
            None,
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            false,
        )
        .unwrap();
        let url = url::Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap();
        let at = chrono::Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let (_, authorization, hash, stamp) = s3.sign("GET", &url, b"", at);
        assert_eq!(stamp, "20130524T000000Z");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(authorization.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="
        ));
        let key = [b"us-east-1".as_slice(), b"s3", b"aws4_request"]
            .iter()
            .fold(
                hmac(b"AWS4wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", b"20130524"),
                |key, part| hmac(&key, part),
            );
        assert_eq!(
            hex::encode(key),
            "dbb893acc010964918f1fd433add87c70e8b0db6be30c1fbeafefa5ec6ba8378"
        );
    }

    #[test]
    fn addresses_are_virtual_hosted_or_path_style() {
        let s3 = |path_style| {
            S3::new(
                "http://127.0.0.1:8333",
                "frood",
                None,
                Some("files/"),
                "k",
                "s",
                path_style,
            )
            .unwrap()
        };
        let key = "a".repeat(64);
        assert_eq!(
            s3(true).url(&key).unwrap().as_str(),
            format!("http://127.0.0.1:8333/frood/files/{key}")
        );
        let hosted = S3::new("https://s3.example", "frood", None, None, "k", "s", false).unwrap();
        assert_eq!(
            hosted.url(&key).unwrap().as_str(),
            format!("https://frood.s3.example/{key}")
        );
    }

    #[tokio::test]
    async fn a_folder_keeps_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let objects = Objects::Disk {
            root: dir.path().to_owned(),
        };
        let data = b"JPEG-board";
        let key = hex::encode(Sha256::digest(data));
        objects.put(&key, data).await.unwrap();
        objects.put(&key, data).await.unwrap();
        assert_eq!(objects.get(&key).await.unwrap(), data);
        assert!(dir.path().join(&key[..2]).join(&key).exists());
        assert!(objects.get(&"0".repeat(64)).await.is_err());
        assert!(objects.put("../escape", data).await.is_err());
    }

    /// Against a real S3-compatible storage: FROOD_TEST_S3 is `endpoint bucket key secret`
    /// (path style); skipped without it.
    #[tokio::test]
    async fn a_real_storage_takes_and_gives_back_a_file() {
        let Ok(spec) = std::env::var("FROOD_TEST_S3") else {
            eprintln!("FROOD_TEST_S3 is not set; skipping");
            return;
        };
        let parts: Vec<&str> = spec.split_whitespace().collect();
        let [endpoint, bucket, key_id, secret] = parts[..] else {
            panic!("FROOD_TEST_S3 must be: endpoint bucket key secret");
        };
        let objects = Objects::S3(
            S3::new(endpoint, bucket, None, Some("test/"), key_id, secret, true).unwrap(),
        );
        let data = format!("a file at {}", chrono::Utc::now()).into_bytes();
        let key = hex::encode(Sha256::digest(&data));
        objects.put(&key, &data).await.unwrap();
        assert_eq!(objects.get(&key).await.unwrap(), data);
        let missing = objects.get(&"0".repeat(64)).await.unwrap_err().to_string();
        assert!(missing.contains("404"), "{missing}");
        let wrong = Objects::S3(
            S3::new(endpoint, bucket, None, None, key_id, "not-the-secret", true).unwrap(),
        );
        let refused = wrong.put(&key, &data).await.unwrap_err().to_string();
        assert!(refused.contains("403"), "{refused}");
    }
}

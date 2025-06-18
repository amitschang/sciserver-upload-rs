use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use reqwest::header::HeaderMap;
use reqwest::{Client, StatusCode};
use tokio::io::AsyncSeekExt;
use tokio::task::JoinSet;
use tokio::fs::File;


enum ErrorKind {
    ReadError,
    FileExists,
    Unauthorized,
    Other,
}

#[allow(dead_code)]
struct UploadInfo {
    path: String,
    time: f64,
    bytes: u64,
    error: Option<ErrorKind>,
    retries: usize,
    _timer: Instant,
}

impl UploadInfo {
    fn new(path: String) -> Self {
        UploadInfo { path, time: 0.0, bytes: 0, error: Some(ErrorKind::Other), retries: 0, _timer: Instant::now() }
    }

    fn set_bytes(&mut self, bytes: u64) {
        self.bytes = bytes;
    }

    fn with_success(self) -> Self {
        let time = self._timer.elapsed().as_secs_f64();
        UploadInfo { error: None, time, ..self }
    }

    fn with_error(self, kind: ErrorKind) -> Self {
        UploadInfo { error: Some(kind), ..self }
    }

    fn incr_retries(&mut self) -> usize {
        self.retries += 1;
        self.retries
    }

}

async fn file_info(file_path: &str) -> Option<(File, &str, u64)> {
    if let Ok(file) = File::open(file_path).await {
        let metadata = file.metadata().await.unwrap();
        if !metadata.is_file() {
            return None;
        }
        let file_name = match Path::new(file_path).file_name() {
            Some(name) => match name.to_str() {
                Some(name) => name,
                _ => return None,
            }
            _ => return None,
        };
        return Some((file, file_name, metadata.len()));
    }
    None
}

async fn upload_file(client: Client, file_path: String, settings: Arc<Settings>) -> UploadInfo {
    let mut info = UploadInfo::new(file_path.clone());
    let (file, file_name) = match file_info(&file_path).await {
        Some((file, name, bytes)) => { info.set_bytes(bytes); (file, name) },
        None => return info.with_error(ErrorKind::ReadError),
    };
    let mut url = format!("{}/{}", settings.prefix, file_name);
    if settings.overwrite {
        url = format!("{}?quiet=true", url);
    }
    loop {
        let file_try = match file.try_clone().await {
            Ok(mut f) => match f.rewind().await {
                Ok(_) => f,
                _ => continue,
            },
            _ => continue,
        };
        let result = client.put(&url).body(file_try).send().await;
        if let Ok(response) = result {
            match response.status() {
                StatusCode::OK => { return info.with_success(); },
                StatusCode::INTERNAL_SERVER_ERROR => {
                    if response.text().await.unwrap().contains("File already exists") {
                        return info.with_error(ErrorKind::FileExists);
                    }
                },
                StatusCode::UNAUTHORIZED => return info.with_error(ErrorKind::Unauthorized),
                _ => (), // retryable
            }
        }
        if info.incr_retries() >= settings.retries {
            return info.with_error(ErrorKind::Other);
        }
    }
}

struct UploadProgress {
    total: usize,
    success: usize,
    error: usize,
    n_retries: usize,
    f_retries: usize,
    bytes: u64,
    timer: Instant,
}

impl UploadProgress {
    fn new(total: usize) -> Self {
        UploadProgress {
            total,
            success: 0,
            error: 0,
            n_retries: 0,
            f_retries: 0,
            bytes: 0,
            timer: Instant::now(),
        }
    }

    fn update(&mut self, info: &UploadInfo, write_status: bool) {
        if info.error.is_some() {
            self.error += 1;
        }
        else {
            self.success += 1;
            self.bytes += info.bytes;
        }
        if info.retries > 0 {
            self.n_retries += info.retries;
            self.f_retries += 1;
        }
        if write_status {
            self.write_status_bar();
        }
    }

    fn status_bar(&self) -> String {
        let elapsed = self.timer.elapsed().as_secs_f64();
        let mbs = self.bytes as f64 / (1024.0 * 1024.0);
        let mbps = mbs / (elapsed + 1e-6);

        format!("Uploaded {}/{} files, {} errors {}|{} retries {:.2} MB in {:.2} seconds ({:.2} MB/s)",
               self.success, self.total, self.error, self.f_retries, self.n_retries, mbs, elapsed, mbps)
    }

    fn write_status_bar(&self) {
        let msg = self.status_bar();
        print!("\r{}", msg);
        io::stdout().flush().unwrap();
    }
}

pub struct Settings {
    prefix: String,
    token: String,
    concurrency: usize,
    retries: usize,
    overwrite: bool,
}

impl Settings {
    pub fn new(prefix: String, token: String, concurrency: usize, retries: usize, overwrite: bool) -> Arc<Self> {
        Arc::new(Settings {
            prefix,
            token,
            concurrency,
            retries,
            overwrite,
        })
    }
}

/// upload many files concurrently
pub async fn upload_many(files: Vec<String>, settings: Arc<Settings>) {
    let mut headers = HeaderMap::new();
    headers.insert("x-auth-token", settings.token.parse().unwrap());
    let client = Client::builder().default_headers(headers).build().unwrap();

    let mut progress = UploadProgress::new(files.len());
    progress.status_bar();

    let mut files_iter = files.into_iter();
    let mut tasks = JoinSet::new();
    // Start with the number of tasks equal to the concurrency limit, then feed
    // in new tasks as they complete, on-by-one to establish as limit.
    for _ in 0..settings.concurrency {
        if let Some(file) = files_iter.next() {
            tasks.spawn(upload_file(client.clone(), file, settings.clone()));
        } else {
            break;
        }
    }
    // main loop, will run into complete or stopped early due to unrecoverable
    // error, feeding in new files as each upload completes. Progress updates
    // emitted with each completed upload.
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(info) => {
                // Early stoppage since unath is expected to cause errors in all
                // other uploads using the same token.
                if let Some(ErrorKind::Unauthorized) = info.error {
                    eprintln!("Unauthorized: Check your token.");
                    return;
                }
                // TODO: could also stop if the error rate after some point is too high
                progress.update(&info, true)
            },
            Err(e) => { eprintln!("Join Error: {:?}", e); }
        }
        if let Some(file) = files_iter.next() {
            tasks.spawn(upload_file(client.clone(), file, settings.clone()));
        }
    }
    println!();
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar() {
        let mut progress = UploadProgress::new(10);
        // regular success and error uploads
        progress.update(&UploadInfo::new("test1.txt".to_string()).with_success(), false);
        progress.update(&UploadInfo::new("test2.txt".to_string()).with_error(ErrorKind::Other), false);
        progress.update(&UploadInfo::new("test3.txt".to_string()).with_success(), false);
        // upload with retries
        let mut info = UploadInfo::new("test4.txt".to_string());
        info.incr_retries();
        info.incr_retries();
        progress.update(&info.with_success(), false);
        let status = progress.status_bar();
        // timing is not deterministic, so we just check the beginning prior to
        // time info
        assert!(status.starts_with("Uploaded 3/10 files, 1 errors 1|2 retries 0.00 MB"));
    }

    #[tokio::test]
    async fn test_file_info() {
        let info = file_info("paththatdoesnotexist.txt").await;
        assert!(info.is_none());
        let tempdir = tempfile::tempdir().unwrap();
        let file_path = tempdir.path().join("testfile.txt");
        std::fs::write(file_path, "Hello, world!").unwrap();
        if let Some((_, name, bytes)) = file_info("testfile.txt").await {
            assert_eq!(name, "testfile.txt");
            assert_eq!(bytes, 13);
        } else {
            panic!("File info should not be None");
        }
    }
}

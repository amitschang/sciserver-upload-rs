use std::io::{self, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use reqwest::header::HeaderMap;
use reqwest::{Body, Client, StatusCode};
use tokio::io::{AsyncRead, AsyncSeekExt, ReadBuf};
use tokio::task::JoinSet;
use tokio::fs::File;
use tokio_util::io::ReaderStream;


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

/// Wraps a file reader to track the number of bytes read and update the global
/// shared bytes_streamed counter for progress tracking. This enables us to
/// report progress even during the upload of larger files.
struct ProgressReader {
    inner: File,
    bytes_streamed: Arc<AtomicU64>,
    bytes_read: Arc<AtomicU64>,
}

impl ProgressReader {
    fn new(inner: File, bytes_streamed: Arc<AtomicU64>) -> (Self, Arc<AtomicU64>) {
        let bytes_read = Arc::new(AtomicU64::new(0));
        (ProgressReader { inner, bytes_streamed, bytes_read: bytes_read.clone() }, bytes_read)
    }
}

/// Implementing `AsyncRead` for `ProgressReader` allows it to be converted into a
/// streamed `reqwest::Body` while tracking upload progress.
impl AsyncRead for ProgressReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let n = (buf.filled().len() - before) as u64;
            self.bytes_streamed.fetch_add(n, Ordering::Relaxed);
            self.bytes_read.fetch_add(n, Ordering::Relaxed);
        }
        result
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

async fn upload_file(
    client: Client, file_path: String, settings: Arc<Settings>, bytes_streamed: Arc<AtomicU64>
) -> UploadInfo {
    let mut info = UploadInfo::new(file_path.clone());
    let (file, file_name, file_size) = match file_info(&file_path).await {
        Some((file, name, bytes)) => { info.set_bytes(bytes); (file, name, bytes) },
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
        let (reader, counter) = ProgressReader::new(file_try, bytes_streamed.clone());
        let body = Body::wrap_stream(ReaderStream::new(reader));
        let result = client.put(&url)
            .header("content-length", file_size)
            .body(body)
            .send()
            .await;
        if let Ok(response) = result {
            if response.status() != StatusCode::OK {
                // If the upload failed, we need to subtract the optimistically
                // added bytes for this attempt before checking the error and
                // potentially retrying, since any next attempt will re-add from
                // the start of the file.
                bytes_streamed.fetch_sub(counter.load(Ordering::Relaxed), Ordering::Relaxed);
            }
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
        } else {
            // in case some got read but the request itself errored
            bytes_streamed.fetch_sub(counter.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        if info.incr_retries() >= settings.retries {
            return info.with_error(ErrorKind::Other);
        }
    }
}

struct UploadProgress {
    n_total: usize,
    n_successes: usize,
    n_errors: usize,
    n_retries: usize,
    f_retries: usize,
    bytes: u64,
    timer: Instant,
    completed: Vec<UploadInfo>,
}

impl UploadProgress {
    fn new(n_total: usize) -> Self {
        UploadProgress {
            n_total,
            n_successes: 0,
            n_errors: 0,
            n_retries: 0,
            f_retries: 0,
            bytes: 0,
            timer: Instant::now(),
            completed: Vec::with_capacity(n_total),
        }
    }

    fn update(&mut self, info: UploadInfo) {
        if info.error.is_some() {
            self.n_errors += 1;
        }
        else {
            self.n_successes += 1;
            self.bytes += info.bytes;
        }
        if info.retries > 0 {
            self.n_retries += info.retries;
            self.f_retries += 1;
        }

        self.completed.push(info);
    }

    fn status_bar(&self, bytes_in_progress: u64) -> String {
        let elapsed = self.timer.elapsed().as_secs_f64();
        let mbs = bytes_in_progress as f64 / (1024.0 * 1024.0);
        let mbps = mbs / (elapsed + 1e-6);

        format!("Uploaded {}/{} files, {} errors {}|{} retries {:.2} MB in {:.2} seconds ({:.2} MB/s)",
               self.n_successes, self.n_total, self.n_errors, self.f_retries, self.n_retries, mbs, elapsed, mbps)
    }

    fn write_status_bar(&self, bytes_in_progress: u64) {
        let msg = self.status_bar(bytes_in_progress);
        print!("\r{}", msg);
        io::stdout().flush().unwrap();
    }

    fn write_error_report(&self) {
        let mut heading_written = false;
        for info in &self.completed {
            if let Some(error) = &info.error {
                if !heading_written {
                    eprintln!("Error Report:");
                    heading_written = true;
                }
                match error {
                    ErrorKind::ReadError => eprintln!(
                        "  Failed to read file: {}", info.path),
                    ErrorKind::FileExists => eprintln!(
                        "  File already exists (use --force to overwrite): {}", info.path),
                    ErrorKind::Unauthorized => eprintln!(
                        "  Unauthorized (check your token): {}", info.path),
                    ErrorKind::Other => eprintln!(
                        "  Failed to upload file after {} retries: {}", info.retries, info.path),
                }
            }
        }
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
    if files.is_empty() {
        eprintln!("No files to upload.");
        return;
    }

    let mut headers = HeaderMap::new();
    headers.insert("x-auth-token", settings.token.parse().unwrap());
    let client = Client::builder().default_headers(headers).build().unwrap();

    let bytes_streamed = Arc::new(AtomicU64::new(0));
    let mut progress = UploadProgress::new(files.len());
    progress.write_status_bar(0);

    let mut files_iter = files.into_iter();
    let mut tasks = JoinSet::new();
    // Start with the number of tasks equal to the concurrency limit, then feed
    // in new tasks as they complete, on-by-one to establish as limit.
    for _ in 0..settings.concurrency {
        if let Some(file) = files_iter.next() {
            tasks.spawn(upload_file(client.clone(), file, settings.clone(), bytes_streamed.clone()));
        } else {
            break;
        }
    }
    // main loop, will run until complete or stopped early due to unrecoverable
    // error, feeding in new files as each upload completes. Progress updates
    // emitted with each completed upload and on a periodic timer.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(180));
    loop {
        tokio::select! {
            result = tasks.join_next() => {
                let Some(result) = result else { break };
                match result {
                    Ok(info) => {
                        // Early stoppage since unauth is expected to cause errors in all
                        // other uploads using the same token.
                        if let Some(ErrorKind::Unauthorized) = info.error {
                            eprintln!("\nUnauthorized: Check your token.");
                            progress.write_error_report();
                            return;
                        }
                        // TODO: could also stop if the error rate after some point is too high
                        progress.update(info);
                        progress.write_status_bar(bytes_streamed.load(Ordering::Relaxed));
                    },
                    Err(e) => { eprintln!("Unexpected Join Error: {:?}", e); }
                }
                if let Some(file) = files_iter.next() {
                    tasks.spawn(upload_file(client.clone(), file, settings.clone(), bytes_streamed.clone()));
                }
            }
            _ = tick.tick() => {
                progress.write_status_bar(bytes_streamed.load(Ordering::Relaxed));
            }
        }
    }
    println!();
    progress.write_error_report();
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar() {
        let mut progress = UploadProgress::new(10);
        // regular success and error uploads
        progress.update(UploadInfo::new("test1.txt".to_string()).with_success());
        progress.update(UploadInfo::new("test2.txt".to_string()).with_error(ErrorKind::Other));
        progress.update(UploadInfo::new("test3.txt".to_string()).with_success());
        // upload with retries
        let mut info = UploadInfo::new("test4.txt".to_string());
        info.incr_retries();
        info.incr_retries();
        progress.update(info.with_success());
        let status = progress.status_bar(0);
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
        std::fs::write(&file_path, "Hello, world!").unwrap();
        if let Some((_, name, bytes)) = file_info(&file_path.to_str().unwrap()).await {
            assert_eq!(name, "testfile.txt");
            assert_eq!(bytes, 13);
        } else {
            panic!("File info should not be None");
        }
    }

    #[tokio::test]
    async fn test_progress_reader() {
        use tokio::io::AsyncReadExt;

        let tempdir = tempfile::tempdir().unwrap();
        let file_path = tempdir.path().join("progress_test.txt");
        let content = vec![b'x'; 1000];
        std::fs::write(&file_path, &content).unwrap();

        let file = File::open(&file_path).await.unwrap();
        let bytes_streamed = Arc::new(AtomicU64::new(0));
        let (mut reader, bytes_read) = ProgressReader::new(file, bytes_streamed.clone());

        assert_eq!(bytes_streamed.load(Ordering::Relaxed), 0);
        assert_eq!(bytes_read.load(Ordering::Relaxed), 0);

        let mut buf = vec![0u8; 256];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 256);
        assert_eq!(bytes_streamed.load(Ordering::Relaxed), 256);
        assert_eq!(bytes_read.load(Ordering::Relaxed), 256);

        // Read the rest
        let mut total = n;
        while total < 1000 {
            let n = reader.read(&mut buf).await.unwrap();
            total += n;
        }
        assert_eq!(bytes_streamed.load(Ordering::Relaxed), 1000);
        assert_eq!(bytes_read.load(Ordering::Relaxed), 1000);

        // EOF returns 0 and counters stay unchanged
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
        assert_eq!(bytes_streamed.load(Ordering::Relaxed), 1000);
    }
}

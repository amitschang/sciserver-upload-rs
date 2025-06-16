use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use reqwest::header::HeaderMap;
use reqwest::{Client, StatusCode};
use tokio::task::JoinSet;
use tokio::fs::File;

struct UploadInfo {
    path: String,
    time: f64,
    bytes: u64,
    error: bool,
}

async fn upload_file(client: Client, file_path: String, settings: Arc<Settings>) -> UploadInfo {
    let timer = std::time::Instant::now();
    let info = UploadInfo { path: file_path.clone(), time: 0.0, error: true, bytes: 0 };
    let file = match File::open(&file_path).await {
        Ok(file) => file,
        Err(_) => return info,
    };
    let info = UploadInfo { bytes: file.metadata().await.unwrap().len(), ..info };
    let file_name = Path::new(&file_path).file_name().unwrap().to_str().unwrap();
    let mut url = format!("{}/{}", settings.prefix, file_name);
    if settings.overwrite {
        url = format!("{}?quiet=true", url);
    }
    let result = client.put(url).body(file).send().await;
    let time = timer.elapsed().as_secs_f64();
    match result {
        Ok(response) => match response.status() {
            StatusCode::OK => UploadInfo { time, error: false, ..info},
            _ => info,
        }
        Err(_) => info,
    }
}

struct UploadProgress {
    total: usize,
    success: usize,
    error: usize,
    bytes: u64,
    timer: Instant,
}

impl UploadProgress {
    fn new(total: usize) -> Self {
        UploadProgress {
            total,
            success: 0,
            error: 0,
            bytes: 0,
            timer: Instant::now(),
        }
    }

    fn update(&mut self, info: &UploadInfo) {
        if info.error {
            self.error += 1;
        }
        else {
            self.success += 1;
            self.bytes += info.bytes;
        }
        self.status_bar();
    }

    fn status_bar(&self) {
        let elapsed = self.timer.elapsed().as_secs_f64();
        let mbs = self.bytes as f64 / (1024.0 * 1024.0);
        let mbps = if elapsed > 0.0 {
            mbs / elapsed
        } else {
            0.0
        };

        print!("\rUploaded {}/{} files, {} errors {:.2} MB in {:.2} seconds ({:.2} MB/s)",
               self.success, self.total, self.error, mbs, elapsed, mbps);
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
    for _ in 0..settings.concurrency {
        if let Some(file) = files_iter.next() {
            tasks.spawn(upload_file(client.clone(), file, settings.clone()));
        } else {
            break;
        }
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(info) => progress.update(&info),
            Err(e) => { eprintln!("Join Error: {:?}", e); }
        }
        if let Some(file) = files_iter.next() {
            tasks.spawn(upload_file(client.clone(), file, settings.clone()));
        }
    }
    println!("");
}

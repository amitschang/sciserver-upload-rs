use std::io::{self, Write};
use std::path::Path;
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

async fn upload_file(client: Client, file_path: String, dest_prefix: String) -> UploadInfo {
    let timer = std::time::Instant::now();
    let info = UploadInfo { path: file_path.clone(), time: 0.0, error: true, bytes: 0 };
    let file = match File::open(&file_path).await {
        Ok(file) => file,
        Err(_) => return info,
    };
    let info = UploadInfo { bytes: file.metadata().await.unwrap().len(), ..info };
    let file_name = Path::new(&file_path).file_name().unwrap().to_str().unwrap();
    let url = format!("{}/{}?quiet=true", dest_prefix, file_name);
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

pub async fn upload_many(files: Vec<String>, dest_prefix: String, token: String, nc: usize) {
    let mut headers = HeaderMap::new();
    headers.insert("x-auth-token", token.parse().unwrap());
    let client = Client::builder().default_headers(headers).build().unwrap();

    let mut progress = UploadProgress::new(files.len());
    progress.status_bar();

    let mut files_iter = files.into_iter();
    let mut tasks = JoinSet::new();
    let dest_prefix = dest_prefix.to_string();
    for _ in 0..nc {
        if let Some(file) = files_iter.next() {
            tasks.spawn(upload_file(client.clone(), file, dest_prefix.clone()));
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
            tasks.spawn(upload_file(client.clone(), file, dest_prefix.clone()));
        }
    }
    println!("");
}

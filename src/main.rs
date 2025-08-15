use clap::Parser;
use upload::{upload_many, Settings};

#[derive(Parser)]
struct Args {
    /// sciserver fileservice http endpoint, defaults to that of jhu-prod
    #[clap(short, long)]
    endpoint: Option<String>,
    /// sciserver token, defaults to SCISERVER_TOKEN env var
    #[clap(short, long, env = "SCISERVER_TOKEN")]
    token: Option<String>,
    /// number of concurrent uploads, defaults to 10
    #[clap(short, long)]
    cons: Option<usize>,
    /// number of retries for each upload, defaults to 3
    #[clap(short, long)]
    retries: Option<usize>,
    /// overwrite existing files, defaults to false
    #[clap(short, long)]
    force: bool,
    /// path to upload files to
    path: String,
    /// files to upload
    files: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let endpoint = args.endpoint.unwrap_or("https://apps.sciserver.org/fileservice/api/file".to_string());
    let prefix = format!("{}/{}", endpoint.trim_matches('/'), args.path.trim_matches('/'));
    let cons = args.cons.unwrap_or(10);
    let retries = args.cons.unwrap_or(3);
    let token = args.token.expect("token not set");

    let settings = Settings::new(
        prefix,
        token.clone(),
        cons,
        retries,
        args.force
    );

    upload_many(args.files, settings).await;
}


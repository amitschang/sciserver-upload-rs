use std::env;

use clap::Parser;
use upload::{upload_many, Settings};

#[derive(Parser)]
struct Args {
    #[clap(short, long)]
    endpoint: Option<String>,
    #[clap(short, long)]
    token: Option<String>,
    #[clap(short, long)]
    cons: Option<usize>,
    #[clap(short, long)]
    force: bool,
    path: String,
    files: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let endpoint = args.endpoint.unwrap_or("https://apps.sciserver.org/fileservice/api/file".to_string());
    let prefix = format!("{}/{}", endpoint.trim_matches('/'), args.path.trim_matches('/'));
    let cons = args.cons.unwrap_or(10);
    let token = args.token.unwrap_or_else(|| env::var("SCISERVER_TOKEN").expect("token not set"));

    let settings = Settings::new(
        prefix,
        token.clone(),
        cons,
        3,
        args.force
    );

    upload_many(args.files, settings).await;
}


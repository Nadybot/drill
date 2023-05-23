use clap::Parser;
use http::Uri;
use log::LevelFilter;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Port of the local HTTP server to tunnel to.
    #[arg(short, long)]
    pub port: u16,

    /// Desired subdomain.
    #[arg(short, long)]
    pub subdomain: String,

    /// URL of the remote drill server entrypoint.
    #[arg(short, long)]
    pub drill: Uri,

    /// Token for static token authentication.
    #[arg(short, long)]
    pub token: Option<String>,

    /// Log level.
    #[arg(long, default_value_t = LevelFilter::Info)]
    pub log_level: LevelFilter,
}

use blob_share::{App, Args};
use clap::Parser;
use dotenv::dotenv;
use eyre::Result;
use tracing_log::LogTracer;
use tracing_subscriber::{self, EnvFilter, FmtSubscriber};

#[actix_web::main]
async fn main() -> Result<()> {
    dotenv().ok();

    LogTracer::init()?;

    // A simple logger that outputs to stdout
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        // .compact()
        // .with_file(true)
        // .with_line_number(true)
        // .with_thread_ids(true)
        // .with_target(true)
        .finish();

    // Set the subscriber as the global default
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // env_logger::init_from_env(env_logger::Env::new().default_filter_or("info,blob_share=debug"));

    let args = Args::parse();
    let app = App::build(args).await?;
    app.run().await?;
    Ok(())
}

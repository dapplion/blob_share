use blob_share::{App, Args};
use clap::Parser;
use eyre::Result;

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info,blob_share=debug"));

    let args = Args::parse();
    let app = App::build(args).await?;
    app.run().await?;
    Ok(())
}

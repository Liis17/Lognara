use lognara_core::config::Config;

fn main() -> anyhow::Result<()> {
    let _config = Config::from_env()?;
    anyhow::bail!("storage initialization is not implemented yet")
}

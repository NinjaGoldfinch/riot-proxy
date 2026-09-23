//! `riot-proxy key create|list|revoke` — the v1 `npm run key:create` flow, in-binary.

use clap::Subcommand;

use crate::config::Config;
use crate::consumers::{self, DEFAULT_QUOTA_PER_MIN, NewConsumer, Scope};

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Mint a consumer key. The plaintext is printed once and never stored.
    Create {
        /// Unique consumer name, e.g. my-website
        #[arg(long, short)]
        name: String,
        /// Comma-separated scopes: read, admin
        #[arg(long, short, value_delimiter = ',', default_value = "read")]
        scopes: Vec<Scope>,
        /// Requests per minute this consumer may make
        #[arg(long, short, default_value_t = DEFAULT_QUOTA_PER_MIN)]
        quota: u32,
    },
    /// List consumers (never their keys).
    List,
    /// Revoke a consumer's key by id or name. The row is kept so the key is never reissued.
    Revoke {
        /// Consumer id or name
        consumer: String,
    },
}

pub async fn run(config: &Config, cmd: KeyCommand) -> anyhow::Result<()> {
    let db = super::open_db(config).await?;
    match cmd {
        KeyCommand::Create { name, scopes, quota } => {
            let created = consumers::create(
                &db,
                NewConsumer {
                    name,
                    scopes,
                    quota_per_min: quota,
                    key: None,
                },
            )
            .await?;
            let c = &created.consumer;
            println!();
            println!("  Consumer created");
            println!("  ────────────────");
            println!("  id        {}", c.id);
            println!("  name      {}", c.name);
            println!("  scopes    {}", join(&c.scopes));
            println!("  quota     {}/min", c.quota_per_min);
            println!();
            println!("  API KEY   {}", created.key.expose());
            println!();
            println!("  This key is shown once and cannot be recovered. Store it now.");
            println!();
        }
        KeyCommand::List => {
            let all = consumers::list(&db).await?;
            println!(
                "{:<26}  {:<24}  {:<10}  {:>6}  STATUS",
                "ID", "NAME", "SCOPES", "QUOTA"
            );
            for c in all {
                let status = if c.revoked_at.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                println!(
                    "{:<26}  {:<24}  {:<10}  {:>6}  {status}",
                    c.id,
                    c.name,
                    join(&c.scopes),
                    c.quota_per_min
                );
            }
        }
        KeyCommand::Revoke { consumer } => {
            let c = consumers::revoke(&db, consumer).await?;
            println!("revoked {} ({})", c.name, c.id);
        }
    }
    Ok(())
}

fn join(scopes: &[Scope]) -> String {
    scopes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

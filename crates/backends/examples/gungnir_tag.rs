//! Set or read gungnird `metadata::*` keys over protocol v2 (a developer tool for
//! trying the live backend without the GIO module).
//!
//! ```sh
//! cargo run -p backends --example gungnir_tag -- /path/to/file emblems important viking
//! cargo run -p backends --example gungnir_tag -- /path/to/file          # list keys
//! ```

use std::path::PathBuf;

use backends::GungnirVfsBackend;
use backends::gungnir::proto::{Value, socket_path_from_env};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: gungnir_tag <absolute path> [key value...]");
        std::process::exit(2);
    };
    let path = std::fs::canonicalize(PathBuf::from(path))?;
    let b = GungnirVfsBackend::connect(socket_path_from_env()?).await?;
    let id = b.identify(&path).await?;
    if let Some(key) = args.get(1) {
        let values = &args[2..];
        let v = match values {
            [one] => Value::String(one.clone()),
            many => Value::Stringv(many.to_vec()),
        };
        b.set(&id, key, &v).await?;
    }
    for k in b.list_keys(&id).await? {
        let v = b.get(&id, &k).await?.map(|v| v.display()).unwrap_or_default();
        println!("metadata::{k} = {v}");
    }
    Ok(())
}

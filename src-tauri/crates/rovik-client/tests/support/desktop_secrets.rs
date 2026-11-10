use std::path::Path;

pub fn seed_secrets(config: &Path) {
    use rovik_client::secrets::{Kdf, SecretStore};
    std::fs::create_dir_all(config).unwrap();
    SecretStore::file(
        config.join("secrets.json"),
        Kdf {
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        },
    )
    .unlock(&rovik_protocol::Secret("test-only".into()))
    .unwrap();
}

pub fn docker_path() -> std::path::PathBuf {
    std::env::var_os("ROVIK_HARNESS_DOCKER_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            if std::path::Path::new("/usr/bin/docker").is_file() {
                "/usr/bin/docker".into()
            } else {
                "/usr/local/bin/docker".into()
            }
        })
}

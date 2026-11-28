//! Unprivileged native-tool adapter installed into the pinned sandbox image.
fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match rovik_harness::tools::execute(&arguments) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(
                if error.category == rovik_harness::FailureCategory::Implementation {
                    42
                } else {
                    2
                },
            );
        }
    }
}

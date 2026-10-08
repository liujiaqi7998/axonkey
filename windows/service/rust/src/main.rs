fn main() {
    if std::env::args().skip(1).any(|arg| arg == "--check") {
        if !axonkey_service::win::abi::verify_layouts() {
            eprintln!("Driver ABI mismatch");
            std::process::exit(1);
        }
        println!(
            "AxonkeyService {}: driver ABI verified; no service or hardware changes made",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    axonkey_service::logging::init();
    if let Err(error) = axonkey_service::win::scm::dispatch() {
        log::error!("Service dispatcher failed: {error}");
        eprintln!("Run this executable through the Windows Service Control Manager: {error}");
        std::process::exit(1);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: ved [file ...]\n\nA small vim-style editor with built-in LSP and an embedded terminal.\nPress :help inside the editor.");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("ved {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Err(e) = ved::app::run(args) {
        eprintln!("ved: {}", e);
        std::process::exit(1);
    }
}

#![warn(clippy::pedantic)]
fn main() {
    let _ = herdsman::SERVE.set((herdsman::hooks::serve, herdsman::serve::export));
    let _ = herdsman::SERVICE.set(herdsman_service::main);
    let ctx = herdsman::Ctx::from_env();
    let code = herdsman::main_with(
        std::env::args().collect(),
        &ctx,
        &herdsman::runner::Real,
        &mut std::io::stdout(),
        &mut std::io::stdin(),
    );
    std::process::exit(code);
}

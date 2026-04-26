use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let initial_path = std::env::args_os().nth(1).map(PathBuf::from);
    reader_ui::run(initial_path)
}

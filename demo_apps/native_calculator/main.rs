#![windows_subsystem = "windows"]

fn main() {
    let script_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("demo_apps/native_calculator/calculator.matrix")))
        .filter(|p| p.is_file())
        .or_else(|| {
            let p = std::path::PathBuf::from("demo_apps/native_calculator/calculator.matrix");
            if p.is_file() {
                Some(p)
            } else {
                None
            }
        });

    let target = match script_path {
        Some(p) => p.to_string_lossy().to_string(),
        None => {
            let temp_dir = std::env::temp_dir().join("spid3r_calculator");
            let _ = std::fs::create_dir_all(&temp_dir);
            let calc_file = temp_dir.join("calculator.matrix");
            let engine_file = temp_dir.join("engine.matrix");
            let _ = std::fs::write(&calc_file, include_str!("calculator.matrix"));
            let _ = std::fs::write(&engine_file, include_str!("engine.matrix"));
            calc_file.to_string_lossy().to_string()
        }
    };

    let _ = spid3r::gui::cmd_gui(&[target]);
}

#![windows_subsystem = "windows"]

fn main() {
    // The tao event loop plus winit/wry/WebView2 debug frames do not fit the
    // 1 MiB main-thread stack (the release build fits; debug frames are much
    // larger), so run the GUI on a roomy worker — the CLI does the same for
    // every subcommand. `window::open` sets tao's `any_thread`, so a worker
    // is fine on Windows; elsewhere the UI must stay on the main thread.
    #[cfg(windows)]
    {
        let handle = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(run_gui)
            .expect("spawn the GUI thread");
        let _ = handle.join();
    }
    #[cfg(not(windows))]
    run_gui();
}

fn run_gui() {
    let script_path = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()
                .map(|p| p.join("examples/demo_apps/native_calculator/calculator.matrix"))
        })
        .filter(|p| p.is_file())
        .or_else(|| {
            let p =
                std::path::PathBuf::from("examples/demo_apps/native_calculator/calculator.matrix");
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

    let _ = spid3r_lang::gui::cmd_gui(&[target]);
}

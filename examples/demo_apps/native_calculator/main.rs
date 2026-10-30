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

    let mut args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let is_dry_run = args.iter().any(|a| a == "--dry-run");
    args.insert(0, target);

    match spid3r_lang::gui::cmd_gui(&args) {
        Ok(code) => {
            if code != 0 {
                std::process::exit(code);
            }
        }
        Err(e) => {
            if is_dry_run {
                eprintln!("{}", e.render());
                std::process::exit(1);
            }
            #[cfg(windows)]
            {
                use std::ffi::OsStr;
                use std::os::windows::ffi::OsStrExt;

                let msg = format!(
                    "Spid3r Calculator encountered a fatal startup error:\n\n{}\n\nIf WebView2 is missing, please install the Microsoft Edge WebView2 Runtime:\nhttps://developer.microsoft.com/microsoft-edge/webview2/",
                    e.render()
                );
                let msg_wide: Vec<u16> = OsStr::new(&msg).encode_wide().chain(Some(0)).collect();
                let title_wide: Vec<u16> = OsStr::new("Spid3r Calculator")
                    .encode_wide()
                    .chain(Some(0))
                    .collect();

                #[link(name = "user32")]
                extern "system" {
                    fn MessageBoxW(
                        hwnd: *mut std::ffi::c_void,
                        lpText: *const u16,
                        lpCaption: *const u16,
                        uType: u32,
                    ) -> i32;
                }

                unsafe {
                    const MB_OK: u32 = 0x00000000;
                    const MB_ICONERROR: u32 = 0x00000010;
                    let _ = MessageBoxW(
                        std::ptr::null_mut(),
                        msg_wide.as_ptr(),
                        title_wide.as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            #[cfg(not(windows))]
            {
                eprintln!("Spid3r Calculator error: {}", e.render());
            }
            std::process::exit(1);
        }
    }
}

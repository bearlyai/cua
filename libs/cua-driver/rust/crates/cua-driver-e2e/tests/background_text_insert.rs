//! Opt-in real-control test. Run with --run-gui in a TCC-authorized Aqua session.
#[cfg(not(target_os = "macos"))]
fn main() {}
#[cfg(target_os = "macos")]
fn main() {
    if std::env::args().any(|s| s == "--check-permissions") {
        native::permission_preflight();
        return;
    }
    if !std::env::args().any(|s| s == "--run-gui") {
        println!("background_text_insert: skipped; pass --run-gui");
        return;
    }
    native::permission_preflight();
    native::run();
}
#[cfg(target_os = "macos")]
mod native {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    use serde_json::{json, Value};
    use std::{
        path::Path,
        process::Command,
        sync::mpsc,
        time::{Duration, Instant},
    };
    enum HarnessDriver {
        Embedded(tokio::runtime::Runtime, cua_driver_core::tool::ToolRegistry),
        Installed(cua_driver_testkit::McpDriver),
    }
    impl HarnessDriver {
        fn new() -> Self {
            if std::env::args().any(|s| s == "--installed-driver") {
                let socket = std::env::var("CUA_E2E_MACOS_DAEMON_SOCKET").unwrap_or_else(|_| {
                    format!(
                        "{}/Library/Caches/cua-driver/cua-driver.sock",
                        std::env::var("HOME").unwrap()
                    )
                });
                Self::Installed(
                    cua_driver_testkit::McpDriver::spawn_daemon_proxy_unrecorded(&socket)
                        .expect("installed TCC-authorized driver must already be running"),
                )
            } else {
                Self::Embedded(
                    tokio::runtime::Runtime::new().unwrap(),
                    platform_macos::register_tools(),
                )
            }
        }
        fn call(&mut self, name: &str, args: Value) -> Value {
            match self {
                Self::Embedded(runtime, registry) => {
                    serde_json::to_value(runtime.block_on(registry.invoke(name, args))).unwrap()
                }
                Self::Installed(driver) => {
                    use cua_driver_testkit::Driver;
                    driver.call(name, args).raw["result"].clone()
                }
            }
        }
    }

    struct Fixture(i32);
    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGTERM);
            }
        }
    }
    fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "fixture timeout");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn state(dir: &Path) -> Value {
        wait(|| {
            std::fs::read(dir.join("state.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        })
    }
    fn command(dir: &Path, text: &str, n: &mut usize) {
        *n += 1;
        std::fs::write(dir.join("command"), format!("{text} {n}")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    pub fn permission_preflight() {
        let result = HarnessDriver::new().call("check_permissions", json!({"prompt":false}));
        println!(
            "permission_preflight={}",
            serde_json::to_string(&result).unwrap()
        );
        let state = &result["structuredContent"];
        assert!(
            state["accessibility"] == true && state["screen_recording"] == true,
            "PERMISSION_PREFLIGHT_STOP: every permission must already be granted"
        );
    }

    pub fn run() {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: bool = msg_send![app,setActivationPolicy:0isize];
            let allocated: *mut AnyObject = msg_send![class!(NSWindow), alloc];
            let window: *mut AnyObject = msg_send![allocated,initWithContentRect:NSRect::new(NSPoint::new(800.,200.),NSSize::new(300.,220.)) styleMask:15u64 backing:2u64 defer:false];
            let _: () = msg_send![window,setReleasedWhenClosed:false];
            let _: () =
                msg_send![window,setTitle:&*NSString::from_str("Caret test foreground sentinel")];
            let _: () = msg_send![window,makeKeyAndOrderFront:std::ptr::null::<AnyObject>()];
            let _: () = msg_send![app, finishLaunching];
            let _: () = msg_send![app,activateIgnoringOtherApps:true];
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(worker);
                tx.send(result).unwrap();
            });
            let result = loop {
                let date: *mut AnyObject =
                    msg_send![class!(NSDate),dateWithTimeIntervalSinceNow:0.01f64];
                let event: *mut AnyObject = msg_send![app,nextEventMatchingMask:u64::MAX untilDate:date inMode:&*NSString::from_str("kCFRunLoopDefaultMode") dequeue:true];
                if !event.is_null() {
                    let _: () = msg_send![app,sendEvent:event];
                }
                let _: () = msg_send![app, updateWindows];
                if let Ok(result) = rx.try_recv() {
                    break result;
                }
            };
            let _: () = msg_send![window, close];
            if let Err(e) = result {
                std::panic::resume_unwind(e)
            }
        }
    }
    fn worker() {
        let dir = match std::env::var_os("CUA_CARET_EVIDENCE_DIR") {
            Some(root) => tempfile::tempdir_in(root).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        let path = dir.path();
        std::fs::write(
            path.join("fixture.swift"),
            include_str!("fixtures/caret_controls.swift"),
        )
        .unwrap();
        let compile = Command::new("swiftc")
            .arg(path.join("fixture.swift"))
            .arg("-o")
            .arg(path.join("fixture"))
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let bundle = path.join("Caret Controls.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::copy(path.join("fixture"), bundle.join("Contents/MacOS/fixture")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"),r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleIdentifier</key><string>ai.bearly.caret-controls-test</string><key>CFBundleName</key><string>Caret Controls</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
        let launch = Command::new("open")
            .args(["-n", "-a"])
            .arg(&bundle)
            .arg("--args")
            .arg(path)
            .arg(std::process::id().to_string())
            .output()
            .unwrap();
        assert!(
            launch.status.success(),
            "open failed: {}",
            String::from_utf8_lossy(&launch.stderr)
        );
        let pid = wait(|| {
            std::fs::read_to_string(path.join("pid"))
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
        });
        let _fixture = Fixture(pid as i32);
        let mut driver = HarnessDriver::new();
        let mut call = |name: &str, args: Value| driver.call(name, args);
        let windows = call("list_windows", json!({"pid":pid}));
        let wid = windows["structuredContent"]["windows"]
            .as_array()
            .expect("window list")
            .iter()
            .find(|w| w["pid"] == pid)
            .and_then(|w| w["window_id"].as_u64())
            .expect("fixture window");
        println!(
            "fixture pid={pid}, window={wid}; windows={}",
            windows["isError"]
        );
        let mut n = 0;
        let background = |n: &mut usize| {
            command(path, "background", n);
            wait(|| {
                let s = state(path);
                (s["active"] == false && s["front_pid"] == std::process::id()).then_some(s)
            });
        };
        for kind in ["field", "search", "combo", "rich", "swift"] {
            for case in [
                "fresh_end",
                "caret",
                "refuse_selection",
                "selection",
                "empty",
                "fresh_caret",
                "fresh_selection",
                "empty_focused",
            ] {
                command(path, "reset", &mut n);
                if case == "empty" || case == "empty_focused" {
                    command(path, &format!("empty {kind}"), &mut n);
                }
                if case == "empty_focused" {
                    command(path, &format!("focus {kind} 0 0"), &mut n);
                }
                if !matches!(
                    case,
                    "fresh_end" | "empty" | "empty_focused" | "fresh_caret" | "fresh_selection"
                ) {
                    command(
                        path,
                        &format!("focus {kind} 3 {}", if case == "caret" { 0 } else { 2 }),
                        &mut n,
                    );
                }
                background(&mut n);
                let before = state(path);
                let snapshot = call(
                    "get_window_state",
                    json!({"pid":pid,"window_id":wid,"include_screenshot":false}),
                );
                let elements = snapshot["structuredContent"]["elements"]
                    .as_array()
                    .expect("elements");
                let entry = elements
                    .iter()
                    .find(|e| e["label"] == kind && e["role"] != "AXButton")
                    .unwrap_or_else(|| panic!("missing {kind}: {elements:?}"));
                assert!(entry.get("focused").is_some(), "missing focus: {entry}");
                if !matches!(
                    case,
                    "fresh_end" | "empty" | "empty_focused" | "fresh_caret" | "fresh_selection"
                ) {
                    assert_eq!(
                        entry["selected_text_range"],
                        json!({"location":3,"length":if case=="caret" {0}else{2}}),
                        "selection observation for {kind}"
                    );
                }
                let mut args = json!({"pid":pid,"window_id":wid,"element_token":entry["element_token"],"text":"!"});
                if case == "fresh_end" {
                    args["insert_at"] = json!("end");
                }
                if case == "selection" || case == "fresh_selection" {
                    args["insert_at"] = json!("selection");
                }
                let result = call("type_text", args);
                if kind == "field" && case == "fresh_caret" {
                    println!("refusal_example={result}");
                }
                let s = &result["structuredContent"];
                std::thread::sleep(Duration::from_millis(100));
                let after = state(path);
                assert_eq!(
                    after["front_pid"], before["front_pid"],
                    "activated {kind}/{case}: {result}"
                );
                assert_eq!(after["active"], false);
                assert_eq!(after["key"], false, "target window became key");
                assert_eq!(
                    after["key_events"], before["key_events"],
                    "transient key window activation"
                );
                assert_eq!(
                    after["activations"], before["activations"],
                    "transient activation"
                );
                assert_eq!(after["pointer"], before["pointer"], "pointer moved");
                assert_eq!(after["rich_style_survived"], true, "rich formatting lost");
                if case == "empty" || case == "empty_focused" {
                    assert_eq!(entry["value"], "", "readable empty value must be present");
                    assert_eq!(s["effect"], "confirmed", "empty field: {result}");
                    assert_eq!(after[kind], "!");
                } else if case == "refuse_selection" {
                    assert_eq!(s["effect"], "refused", "{kind}/{case}: {result}");
                    assert_eq!(
                        s["code"], "BACKGROUND_TEXT_SELECTION_REQUIRES_EXPLICIT_REPLACEMENT",
                        "{kind}/{case}: {result}"
                    );
                    assert_eq!(after[kind], "Original");
                } else if matches!(case, "fresh_caret" | "fresh_selection") && kind != "rich" {
                    assert_eq!(
                        s["code"], "BACKGROUND_TEXT_FOCUS_SELECTED_VALUE",
                        "{kind}/{case}: {result}"
                    );
                    assert_eq!(after[kind], "Original");
                    assert_eq!(
                        s["range_restored"], true,
                        "the refusal must undo the focus selection: {kind}/{case}: {result}"
                    );
                } else {
                    let expected = match case {
                        "fresh_end" | "fresh_caret" | "fresh_selection" => "Original!",
                        "caret" => "Ori!ginal",
                        "selection" => "Ori!nal",
                        _ => unreachable!(),
                    };
                    assert_eq!(s["effect"], "confirmed", "{kind}/{case}: {result}");
                    assert_eq!(after[kind], expected, "{kind}/{case}: {result}");
                }
                println!("{kind:6} {case:17} effect={} code={} focused_first={} range_restored={} before_range={} value={}", s["effect"],s["code"],s["focused_first"],s["range_restored"],entry["selected_text_range"],after[kind]);
                if matches!(case, "fresh_end" | "empty") {
                    assert_eq!(
                        s["focused_first"], true,
                        "focused_first must survive public projection: {result}"
                    );
                }
            }
        }
        println!("40 control cases passed: independent app values, retained formatting, frontmost app and pointer unchanged");
    }
}

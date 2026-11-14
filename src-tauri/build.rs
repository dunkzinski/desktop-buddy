fn main() {
  tauri_build::build();

  // Give the **test** binaries the same Windows manifest as the app.
  //
  // `tauri_build::build()` writes `resource.lib` (version info, icon and the
  // `Microsoft.Windows.Common-Controls` v6 manifest) into this crate's OUT_DIR
  // and asks cargo to link it into the *bins* only. Test binaries link wry too,
  // and wry imports comctl32 v6-only symbols (`TaskDialogIndirect`,
  // `SetWindowSubclass`, ...). Without the manifest the loader resolves
  // `comctl32.dll` to v5, the process dies before `main` with
  // STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) and `cargo test` reports nothing
  // but that exit code. Linking the same resource lib into the test binaries is
  // what makes the suite runnable.
  if let Ok(out_dir) = std::env::var("OUT_DIR") {
    // `resource.lib` is where tauri-build put the .rc it generated (version
    // info, icon and the `Microsoft.Windows.Common-Controls` v6 manifest).
    // Put it on the search path so `#[link(name = "resource")]` in lib.rs can
    // reach it from the unit-test harness; `rustc-link-arg-tests` cannot, since
    // cargo only applies that to integration-test targets.
    println!("cargo:rustc-link-search=native={out_dir}");
    let resource = std::path::PathBuf::from(&out_dir).join("resource.lib");
    if resource.exists() {
      // Integration tests (tests/*.rs) link the lib as a normal dependency, so
      // they need the resource passed on the command line instead.
      println!("cargo:rustc-link-arg-tests={}", resource.display());
    }
  }
}

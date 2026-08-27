fn main() {
  // 不直接用 tauri_build::build()：默认实现会把 Common-Controls v6 manifest
  // 嵌进 bin 的 .res（仅 bin）。而单测二进制只要构造/析构 EventBus（内含
  // tauri::AppHandle，其 Arc<AppManager> drop 链拉入 tao/muda/tray-icon 的
  // GUI 符号，导入 comctl32.dll 的 TaskDialogIndirect 等 v6-only 导出），
  // 又没有 manifest，加载即 0xC0000139（ENTRYPOINT_NOT_FOUND）。
  // 因此：.res 不带 manifest（保留 icon/版本信息），改由下方 link-arg 把
  // 同一份 manifest 统一嵌入所有链接产物（bin / 测试 exe / cdylib），
  // 避免与 tauri-build 的 .res 在 bin 上双重嵌入（CVT1100 资源重复）。
  let attributes =
    tauri_build::Attributes::new().windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
  if let Err(error) = tauri_build::try_build(attributes) {
    let error = format!("{error:#}");
    println!("{error}");
    std::process::exit(1);
  }

  let target = std::env::var("TARGET").unwrap_or_default();
  if target.contains("windows-msvc") {
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let manifest_path = std::path::Path::new(&out_dir).join("app.exe.manifest");
    std::fs::write(&manifest_path, manifest).expect("write app.exe.manifest");
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
      "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
      manifest_path.display()
    );
  }
}

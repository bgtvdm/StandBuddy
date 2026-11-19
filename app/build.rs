use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    slint_build::compile("ui/app.slint").expect("slint build failed");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resources();
    }
}

/// Exe icon, version info and manifest, compiled with the Windows SDK's rc.exe and passed
/// to the linker as a .res file — no resource crate needed.
fn embed_resources() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = root.join("../assets/tray.ico").canonicalize().expect("assets/tray.ico missing");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-env-changed=RC");

    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> =
        version.split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let numeric = parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",");

    fs::write(out.join("app.manifest"), MANIFEST).unwrap();
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "app.manifest"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "bgtvdm"
      VALUE "FileDescription", "StandBuddy"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "standbuddy"
      VALUE "LegalCopyright", "MIT"
      VALUE "OriginalFilename", "standbuddy.exe"
      VALUE "ProductName", "StandBuddy"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display().to_string().trim_start_matches(r"\\?\").replace('\\', "\\\\"),
    );
    fs::write(out.join("app.rc"), rc).unwrap();

    let res = out.join("app.res");
    let status = Command::new(rc_exe())
        .current_dir(&out)
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg("app.rc")
        .status()
        .expect("failed to run rc.exe");
    assert!(status.success(), "rc.exe failed");
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// `RC` if set, else the newest x64 rc.exe from the Windows 10/11 SDK.
fn rc_exe() -> PathBuf {
    if let Some(rc) = env::var_os("RC") {
        return rc.into();
    }
    let bin = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let newest = fs::read_dir(&bin)
        .expect("Windows SDK not found (set RC to rc.exe)")
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(r"x64\rc.exe"))
        .filter(|p| p.exists())
        .max_by_key(|p| {
            let ver = p.parent().and_then(|x| x.parent()).and_then(|d| d.file_name()).unwrap_or_default();
            ver.to_string_lossy().split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()
        });
    newest.expect("rc.exe not found in the Windows SDK (set RC to rc.exe)")
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

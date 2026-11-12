fn main() {
    let sources = [
        "c_shim.c",
        "vendor/libnetease/src/vendor/base64.c",
        "vendor/libnetease/src/vendor/hex.c",
        "vendor/libnetease/src/vendor/md5.c",
        "vendor/libnetease/src/vendor/aes.c",
        "vendor/libnetease/src/vendor/bignum.c",
        "vendor/libnetease/src/vendor/rsa.c",
        "vendor/libnetease/src/vendor/inflate.c",
        "vendor/libnetease/src/vendor/qrenc.c",
        "vendor/libnetease/src/data/deviceids_data.c",
        "vendor/libnetease/src/core/util.c",
        "vendor/libnetease/src/core/rand.c",
        "vendor/libnetease/src/core/jmap.c",
        "vendor/libnetease/src/core/jval.c",
        "vendor/libnetease/src/core/deviceids.c",
        "vendor/libnetease/src/core/crypto.c",
        "vendor/libnetease/src/core/http.c",
        "vendor/libnetease/src/core/cookiejar.c",
        "vendor/libnetease/src/core/risk.c",
        "vendor/libnetease/src/core/request.c",
        "vendor/libnetease/src/service/qr.c",
        "vendor/libnetease/src/service/services.c",
    ];

    let mut build = cc::Build::new();
    build
        .include("vendor/libnetease/include")
        .define("_CRT_SECURE_NO_WARNINGS", None)
        .files(sources);
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag_if_supported("/utf-8");
    } else {
        build.flag_if_supported("-std=gnu11");
        build.define("_GNU_SOURCE", None);
    }
    build.compile("netease");
    println!("cargo:rerun-if-changed=vendor/libnetease/include");
    println!("cargo:rerun-if-changed=vendor/libnetease/src");

    slint_build::compile("ui/app.slint").expect("failed to compile Slint UI");
    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::path::Path::new("assets/icon.ico").exists() {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set("ProductName", "Decro Slint");
        resource.set("FileDescription", "Decro Slint portable client");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=failed to embed Windows icon: {error}");
        }
    }
}

#[cfg(not(windows))]
fn embed_windows_icon() {}

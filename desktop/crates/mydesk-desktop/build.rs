//! 构建脚本：把仓库根目录的 `MyDesk.html` 复制成 Tauri 的前端入口。
//!
//! 注意路径：manifest 在 desktop/crates/mydesk-desktop，仓库根在 ../../../
//! 这样只有一个事实来源（网页版那 3900 行），不需要维护第二份 UI；
//! 原生版与网页版的差异全部收敛在 `ui/tauri-shim.js` 里。

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("../../../MyDesk.html");
    let ui_dir = root.join("ui");
    let out = ui_dir.join("index.html");

    println!("cargo:rerun-if-changed={}", src.display());
    println!("cargo:rerun-if-changed={}", ui_dir.join("tauri-shim.js").display());

    let html = std::fs::read_to_string(&src)
        .unwrap_or_else(|e| panic!("读不到 {}：{e}", src.display()));
    let shim = "<script src=\"tauri-shim.js\"></script>\n</body>";
    if !html.contains("</body>") {
        panic!("{} 里没有 </body>，无法注入适配层", src.display());
    }
    let patched = html.replacen("</body>", shim, 1);
    std::fs::create_dir_all(&ui_dir).expect("创建 ui/ 失败");
    std::fs::write(&out, patched).expect("写入 ui/index.html 失败");

    tauri_build::build()
}

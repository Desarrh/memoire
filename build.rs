// Icône et nom de l'application dans l'exe.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("FileDescription", "Mémoire")
            // commandes sans fenêtre comprises par cet exe (lu par Bilan avant d'appeler --stats-semaine)
            .set("MemoireCli", "stats-semaine chercher copier")
            .set("ProductName", "Mémoire")
            .set("InternalName", "memoire")
            .set("OriginalFilename", "Memoire.exe")
            .set("CompanyName", "Desarrh")
            .set("LegalCopyright", "MIT");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icône non intégrée : {e}");
        }
    }
}
